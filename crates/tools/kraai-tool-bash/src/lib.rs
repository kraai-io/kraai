#![forbid(unsafe_code)]

use std::time::Duration;

use async_trait::async_trait;
use kraai_command_runner::{CommandRequest, run_command};
use kraai_tool_core::{ToolCallResult, ToolContext, TypedTool};
use kraai_toon_schema::toon_tool;
use kraai_types::{
    ExecutionPolicy, RiskLevel, SandboxMode, SandboxPermissions, ToolCallAssessment,
};
use serde::Serialize;

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
            #[toon_schema(description = "Include stdout and stderr when the command succeeds. Disabled by default; failing commands always return stdout and stderr")]
            include_success_output: bool,

            #[serde(default)]
            #[toon_schema(description = "Sandbox permission override: use_default, require_escalated, or with_additional_permissions")]
            sandbox_permissions: Option<String>,
        }
    },
    root: BashToolArgs,
    examples: [
        { command: ["git", "status", "--short"], timeout_seconds: 10, include_success_output: true },
        { command: ["cargo", "test", "-p", "package"], timeout_seconds: 120 },
        { command: ["echo", "an argument with spaces"], timeout_seconds: 10, include_success_output: true },
        { command: ["rg", "-n", "tool call", "crates"], timeout_seconds: 10, include_success_output: true },
        { command: ["sed", "-n", "1,20p", "crates/tools/kraai-tool-bash/src/lib.rs"], timeout_seconds: 10, include_success_output: true },
        { command: ["git", "status", "--short"], timeout_seconds: 10, sandbox_permissions: "require_escalated", include_success_output: true },
    ]
}

#[derive(Serialize)]
struct BashToolOutput {
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    sandbox_denied: bool,
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

    fn assess(&self, args: &Self::Args, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let Ok(sandbox_permissions) = parse_sandbox_permissions(args) else {
            return ToolCallAssessment {
                risk: RiskLevel::WriteOutsideWorkspace,
                policy: ExecutionPolicy::NeverAllow,
                reasons: vec![format!(
                    "Invalid sandbox permission override for command: {}",
                    args.command.join(" ")
                )],
            };
        };

        if sandbox_permissions == SandboxPermissions::WithAdditionalPermissions {
            return ToolCallAssessment {
                risk: RiskLevel::WriteOutsideWorkspace,
                policy: ExecutionPolicy::NeverAllow,
                reasons: vec![String::from(
                    "sandbox_permissions=with_additional_permissions is not supported yet",
                )],
            };
        }

        if sandbox_permissions.requires_escalated_permissions()
            || ctx.global_config.sandbox.mode == SandboxMode::DangerFullAccess
        {
            return ToolCallAssessment {
                risk: RiskLevel::WriteOutsideWorkspace,
                policy: ExecutionPolicy::AlwaysAsk,
                reasons: vec![format!(
                    "Runs command without sandbox: {}",
                    args.command.join(" ")
                )],
            };
        }

        let risk = match ctx.global_config.sandbox.mode {
            SandboxMode::ReadOnly => RiskLevel::ReadOnlyWorkspace,
            SandboxMode::WorkspaceWrite => RiskLevel::UndoableWorkspaceWrite,
            SandboxMode::External => RiskLevel::UndoableWorkspaceWrite,
            SandboxMode::DangerFullAccess => RiskLevel::WriteOutsideWorkspace,
        };

        ToolCallAssessment {
            risk,
            policy: ExecutionPolicy::AutonomousUpTo(risk),
            reasons: vec![format!(
                "Runs command in {} mode: {}",
                ctx.global_config.sandbox.mode.as_str(),
                args.command.join(" ")
            )],
        }
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolCallResult {
        if args.timeout_seconds == 0 {
            return ToolCallResult::error(String::from(
                "timeout_seconds must be greater than zero",
            ));
        }

        let sandbox_permissions = match parse_sandbox_permissions(&args) {
            Ok(permission) => permission,
            Err(message) => return ToolCallResult::error(message),
        };

        let timeout = Duration::from_secs(u64::from(args.timeout_seconds));
        let output = match run_command(CommandRequest {
            command: args.command.clone(),
            cwd: ctx.global_config.workspace_dir.clone(),
            sandbox: ctx.global_config.sandbox.clone(),
            sandbox_permissions,
            timeout,
        })
        .await
        {
            Ok(output) => output,
            Err(error) => return ToolCallResult::error(error.to_string()),
        };

        let should_include_output = output.exit_code != Some(0) || args.include_success_output;
        let stdout = should_include_output.then_some(output.stdout);
        let stderr = should_include_output.then_some(output.stderr);

        ToolCallResult::success(BashToolOutput {
            exit_code: output.exit_code,
            stdout,
            stderr,
            sandbox_denied: output.sandbox_denied,
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

fn parse_sandbox_permissions(args: &BashToolArgs) -> Result<SandboxPermissions, String> {
    let Some(value) = args.sandbox_permissions.as_deref() else {
        return Ok(SandboxPermissions::UseDefault);
    };

    SandboxPermissions::parse(value).ok_or_else(|| {
        String::from(
            "sandbox_permissions must be one of: use_default, require_escalated, with_additional_permissions",
        )
    })
}

fn is_false(value: &bool) -> bool {
    !*value
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
    use kraai_types::{
        ExecutionPolicy, RiskLevel, SandboxMode, ToolCallGlobalConfig, ToolStateSnapshot,
    };

    use super::{BashTool, BashToolArgs};

    fn tool_config(workspace_dir: &Path) -> ToolCallGlobalConfig {
        let mut config = ToolCallGlobalConfig::new(workspace_dir.to_path_buf());
        config.sandbox.mode = SandboxMode::DangerFullAccess;
        config
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
            include_success_output: false,
            sandbox_permissions: None,
        }
    }

    fn bash_args_with_success_output(command: &[&str], timeout_seconds: u32) -> BashToolArgs {
        BashToolArgs {
            include_success_output: true,
            ..bash_args(command, timeout_seconds)
        }
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
                bash_args_with_success_output(&["pwd"], 5),
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
                bash_args(&["sh", "-c", "printf output; printf failed >&2; exit 7"], 5),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                assert_eq!(data["exit_code"].as_i64(), Some(7));
                assert_eq!(data["stdout"].as_str(), Some("output"));
                assert_eq!(data["stderr"].as_str(), Some("failed"));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn suppresses_success_output_by_default() {
        let workspace_dir = make_temp_dir("suppresses-success-output-by-default");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(
                bash_args(&["sh", "-c", "printf output; printf diagnostic >&2"], 5),
                &tool_context(&config, &snapshot),
            )
            .await;

        match output.output {
            ToolOutput::Success { data } => {
                assert_eq!(data["exit_code"].as_i64(), Some(0));
                assert!(data.get("stdout").is_none());
                assert!(data.get("stderr").is_none());
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn returns_success_output_when_requested() {
        let workspace_dir = make_temp_dir("returns-success-output-when-requested");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();

        let output = tool
            .call(
                bash_args_with_success_output(
                    &["sh", "-c", "printf output; printf diagnostic >&2"],
                    5,
                ),
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
    fn assessment_for_escalated_command_is_always_ask_write_outside_workspace() {
        let workspace_dir = make_temp_dir("assessment");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let mut args = bash_args(&["git", "status"], 5);
        args.sandbox_permissions = Some(String::from("require_escalated"));

        let assessment = tool.assess(&args, &tool_context(&config, &snapshot));

        assert_eq!(assessment.risk, RiskLevel::WriteOutsideWorkspace);
        assert_eq!(assessment.policy, ExecutionPolicy::AlwaysAsk);

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assessment_for_default_workspace_write_sandbox_is_autonomous_workspace_write() {
        let workspace_dir = make_temp_dir("sandbox-assessment");
        let tool = BashTool;
        let config = ToolCallGlobalConfig::new(workspace_dir.clone());
        let snapshot = ToolStateSnapshot::default();

        let assessment = tool.assess(
            &bash_args(&["git", "status"], 5),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::UndoableWorkspaceWrite);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::UndoableWorkspaceWrite)
        );

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assessment_for_external_sandbox_is_autonomous_workspace_write() {
        let workspace_dir = make_temp_dir("external-sandbox-assessment");
        let tool = BashTool;
        let mut config = ToolCallGlobalConfig::new(workspace_dir.clone());
        config.sandbox.mode = SandboxMode::External;
        let snapshot = ToolStateSnapshot::default();

        let assessment = tool.assess(
            &bash_args(&["cargo", "test"], 5),
            &tool_context(&config, &snapshot),
        );

        assert_eq!(assessment.risk, RiskLevel::UndoableWorkspaceWrite);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::UndoableWorkspaceWrite)
        );
        assert!(assessment.reasons[0].contains("external mode"));

        cleanup_temp_dir(&workspace_dir);
    }

    #[tokio::test]
    async fn rejects_invalid_sandbox_permission() {
        let workspace_dir = make_temp_dir("invalid-sandbox-permission");
        let tool = BashTool;
        let config = tool_config(&workspace_dir);
        let snapshot = ToolStateSnapshot::default();
        let mut args = bash_args(&["true"], 1);
        args.sandbox_permissions = Some(String::from("root_please"));

        let output = tool.call(args, &tool_context(&config, &snapshot)).await;

        match output.output {
            ToolOutput::Error { message } => {
                assert!(message.contains("sandbox_permissions must be one of"));
            }
            ToolOutput::Success { .. } => panic!("expected invalid permission error"),
        }

        cleanup_temp_dir(&workspace_dir);
    }

    #[test]
    fn assessment_rejects_unsupported_additional_permissions() {
        let workspace_dir = make_temp_dir("unsupported-additional-permissions");
        let tool = BashTool;
        let config = ToolCallGlobalConfig::new(workspace_dir.clone());
        let snapshot = ToolStateSnapshot::default();
        let mut args = bash_args(&["true"], 1);
        args.sandbox_permissions = Some(String::from("with_additional_permissions"));

        let assessment = tool.assess(&args, &tool_context(&config, &snapshot));

        assert_eq!(assessment.risk, RiskLevel::WriteOutsideWorkspace);
        assert_eq!(assessment.policy, ExecutionPolicy::NeverAllow);

        cleanup_temp_dir(&workspace_dir);
    }
}
