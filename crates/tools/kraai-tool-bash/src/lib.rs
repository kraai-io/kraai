#![forbid(unsafe_code)]

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use kraai_tool_core::{ToolCallResult, ToolContext, TypedTool};
use kraai_toon_schema::toon_tool;
use kraai_types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};
use serde::Serialize;
use tokio::process::Command;

#[derive(Clone, Copy)]
pub struct BashTool;

toon_tool! {
    name: "bash",
    description: "Run an argv-style command from the workspace root",
    types: {
        #[derive(Clone, serde::Deserialize, serde::Serialize)]
        pub struct BashToolArgs {
            #[toon_schema(description = "Executable and arguments to run without shell parsing", min = 1)]
            command: Vec<String>,

            #[toon_schema(description = "Maximum execution time in seconds")]
            timeout_seconds: u32,

            #[serde(default)]
            #[toon_schema(description = "Include stdout in the result. Disabled by default; exit code and stderr are always returned")]
            include_stdout: bool,
        }
    },
    root: BashToolArgs,
    examples: [
        { command: ["git", "status", "--short"], timeout_seconds: 10, include_stdout: true },
        { command: ["cargo", "test", "-p", "package"], timeout_seconds: 120 },
        { command: ["echo", "an argument with spaces"], timeout_seconds: 10, include_stdout: true },
        { command: ["rg", "-n", "tool call", "crates"], timeout_seconds: 10, include_stdout: true },
    ]
}

#[derive(Serialize)]
struct BashToolOutput {
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    stderr: String,
}

#[async_trait]
impl TypedTool for BashTool {
    type Args = BashToolArgs;

    fn name(&self) -> &'static str {
        BashToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        BashToolArgs::toon_schema()
    }

    fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::WriteOutsideWorkspace,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![format!("Runs command: {}", args.command.join(" "))],
        }
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolCallResult {
        if args.command.is_empty() {
            return ToolCallResult::error(String::from("command must contain at least one item"));
        }
        if args.timeout_seconds == 0 {
            return ToolCallResult::error(String::from(
                "timeout_seconds must be greater than zero",
            ));
        }

        let program = &args.command[0];
        let mut command = Command::new(program);
        command
            .args(args.command.get(1..).unwrap_or_default())
            .current_dir(&ctx.global_config.workspace_dir)
            .stdin(Stdio::null())
            .stdout(if args.include_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return ToolCallResult::error(format!(
                    "unable to spawn command '{}': {error}",
                    program
                ));
            }
        };

        let timeout = Duration::from_secs(u64::from(args.timeout_seconds));
        let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return ToolCallResult::error(format!("unable to wait for command: {error}"));
            }
            Err(_) => {
                return ToolCallResult::error(format!(
                    "command timed out after {} second(s)",
                    args.timeout_seconds
                ));
            }
        };

        ToolCallResult::success(BashToolOutput {
            exit_code: output.status.code(),
            stdout: args
                .include_stdout
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned()),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn describe(&self, args: &Self::Args) -> String {
        format!(
            "Run command with {}s timeout: {}",
            args.timeout_seconds,
            args.command.join(" ")
        )
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests use direct assertions for process output fixtures"
)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use kraai_tool_core::{ToolContext, ToolOutput, TypedTool};
    use kraai_types::{ExecutionPolicy, RiskLevel, ToolCallGlobalConfig, ToolStateSnapshot};

    use super::{BashTool, BashToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        ToolCallGlobalConfig {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    fn tool_context<'a>(
        config: &'a ToolCallGlobalConfig,
        snapshot: &'a ToolStateSnapshot,
    ) -> ToolContext<'a> {
        ToolContext {
            global_config: config,
            tool_state_snapshot: snapshot,
        }
    }

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "kraai-tool-bash-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    fn bash_args(command: &[&str], timeout_seconds: u32) -> BashToolArgs {
        BashToolArgs {
            command: command.iter().map(|item| item.to_string()).collect(),
            timeout_seconds,
            include_stdout: false,
        }
    }

    fn bash_args_with_stdout(command: &[&str], timeout_seconds: u32) -> BashToolArgs {
        BashToolArgs {
            include_stdout: true,
            ..bash_args(command, timeout_seconds)
        }
    }

    #[test]
    fn stdout_is_opt_in() {
        let args: BashToolArgs = serde_json::from_value(serde_json::json!({
            "command": ["true"],
            "timeout_seconds": 5,
        }))
        .expect("args deserialize");

        assert!(!args.include_stdout);
        assert!(
            BashToolArgs::toon_schema().contains("include_stdout[1:1]: boolean # default: default")
        );
    }

    #[tokio::test]
    async fn rejects_empty_commands() {
        let workspace_dir = make_temp_dir("rejects-empty-commands");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(bash_args(&[], 1), &tool_context(&config, &snapshot))
            .await;

        match output.output {
            ToolOutput::Error { message } => {
                assert!(message.contains("command must contain at least one item"));
            }
            ToolOutput::Success { .. } => panic!("expected error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn runs_command_in_workspace_directory() {
        let workspace_dir = make_temp_dir("runs-command-in-workspace-directory");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(
                bash_args_with_stdout(&["pwd"], 5),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                assert_eq!(data["exit_code"].as_i64(), Some(0));
                assert_eq!(
                    data["stdout"].as_str().map(str::trim_end),
                    Some(workspace_dir.to_str().expect("utf8 path"))
                );
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn returns_non_zero_exit_as_success_output() {
        let workspace_dir = make_temp_dir("returns-non-zero-exit-as-success-output");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(
                bash_args(&["sh", "-c", "printf failed >&2; exit 7"], 5),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                assert_eq!(data["exit_code"].as_i64(), Some(7));
                assert_eq!(data["stderr"].as_str(), Some("failed"));
                assert!(data.get("stdout").is_none());
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn returns_stdout_when_requested() {
        let workspace_dir = make_temp_dir("returns-stdout-when-requested");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(
                bash_args_with_stdout(&["sh", "-c", "printf output; printf diagnostic >&2"], 5),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                assert_eq!(data["exit_code"].as_i64(), Some(0));
                assert_eq!(data["stdout"].as_str(), Some("output"));
                assert_eq!(data["stderr"].as_str(), Some("diagnostic"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn times_out_commands() {
        let workspace_dir = make_temp_dir("times-out-commands");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(
                bash_args(&["sh", "-c", "sleep 2"], 1),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Error { message } => {
                assert!(message.contains("timed out after 1 second"));
            }
            ToolOutput::Success { .. } => panic!("expected timeout error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assessment_is_always_ask_write_outside_workspace() {
        let workspace_dir = make_temp_dir("assessment");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let assessment = tool.assess(
            &bash_args(&["git", "status"], 5),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::WriteOutsideWorkspace);
        assert_eq!(assessment.policy, ExecutionPolicy::AlwaysAsk);

        cleanup_temp_dir(&workspace_dir);
    }
}
