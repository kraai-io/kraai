#![forbid(unsafe_code)]

mod api;
mod manager;
mod paths;
mod prepared;
pub mod toon_parser;

pub use api::{ToolCallResult, ToolContext, ToolError, ToolOutput, TypedTool};
pub use manager::ToolManager;
pub use paths::{
    ResolvedToolPath, TextFileRead, assess_read_path, assess_write_path,
    format_text_with_line_numbers, normalize_tool_path, path_is_within_workspace, read_text_file,
    read_text_path, resolve_tool_path,
};
pub use prepared::PreparedToolCall;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tool-core tests use direct assertions for filesystem and manager fixtures"
)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    use async_trait::async_trait;
    use kraai_types::{
        ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolCallGlobalConfig, ToolStateSnapshot,
    };
    use serde::ser::{Error as _, Serialize, Serializer};
    use serde::{Deserialize, Deserializer};
    use serde_json::json;

    use super::{
        PreparedToolCall, ToolCallResult, ToolContext, ToolError, ToolManager, ToolOutput,
        TypedTool, assess_read_path, assess_write_path, format_text_with_line_numbers,
        read_text_path, resolve_tool_path,
    };

    fn make_temp_dir(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agent-tool-core-{test_name}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup_temp_dir(path: &PathBuf) {
        let _ = fs::remove_dir_all(path);
    }

    struct FailingSerialize;

    impl Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("intentional failure"))
        }
    }

    #[test]
    fn tool_output_success_falls_back_to_error_on_serialize_failure() {
        let result = ToolCallResult::success(FailingSerialize);

        match result.output {
            ToolOutput::Error { message } => {
                assert!(message.contains("failed to serialize tool output"));
                assert!(message.contains("intentional failure"));
            }
            ToolOutput::Success { .. } => panic!("expected tool serialization failure"),
        }
        assert!(result.tool_state_deltas.is_empty());
    }

    #[test]
    fn resolve_tool_path_marks_parent_traversal_outside_workspace() {
        let workspace_root = Path::new("/tmp/workspace");
        let resolved = resolve_tool_path(workspace_root, "../elsewhere/file.txt");

        assert_eq!(resolved.path(), Path::new("/tmp/elsewhere/file.txt"));
        assert!(!resolved.is_within_workspace());
    }

    #[test]
    fn assess_read_path_uses_workspace_policy_for_inside_paths() {
        let workspace_root = Path::new("/tmp/workspace");
        let assessment = assess_read_path(
            workspace_root,
            "src/lib.rs",
            "Reads workspace file",
            "Reads file outside workspace",
        );

        assert_eq!(assessment.risk, RiskLevel::ReadOnlyWorkspace);
        assert_eq!(
            assessment.policy,
            ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace)
        );
        assert_eq!(
            assessment.reasons,
            vec![String::from(
                "Reads workspace file /tmp/workspace/src/lib.rs"
            )]
        );
    }

    #[test]
    fn format_text_with_line_numbers_uses_one_based_indices() {
        assert_eq!(
            format_text_with_line_numbers("alpha\nbeta\n"),
            "1|alpha\n2|beta"
        );
    }

    #[test]
    fn assess_write_path_uses_write_risk_levels() {
        let workspace_root = Path::new("/tmp/workspace");

        let inside = assess_write_path(
            workspace_root,
            "src/lib.rs",
            "Edits workspace file",
            "Edits file outside workspace",
        );
        assert_eq!(inside.risk, RiskLevel::UndoableWorkspaceWrite);
        assert_eq!(inside.policy, ExecutionPolicy::AlwaysAsk);

        let outside = assess_write_path(
            workspace_root,
            "../elsewhere/file.txt",
            "Edits workspace file",
            "Edits file outside workspace",
        );
        assert_eq!(outside.risk, RiskLevel::WriteOutsideWorkspace);
        assert_eq!(outside.policy, ExecutionPolicy::AlwaysAsk);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_tool_path_treats_symlink_escape_as_outside_workspace() {
        use std::os::unix::fs::symlink;

        let workspace_root = make_temp_dir("symlink-workspace");
        let outside_root = make_temp_dir("symlink-outside");
        let symlink_path = workspace_root.join("outside-link");
        let outside_file = outside_root.join("secret.txt");
        fs::write(&outside_file, "secret").expect("write outside file");
        symlink(&outside_root, &symlink_path).expect("create symlink");

        let resolved = resolve_tool_path(&workspace_root, "outside-link/secret.txt");

        assert_eq!(
            resolved.path(),
            workspace_root.join("outside-link/secret.txt")
        );
        assert!(!resolved.is_within_workspace());

        cleanup_temp_dir(&workspace_root);
        cleanup_temp_dir(&outside_root);
    }

    #[derive(Clone)]
    struct SpyTool {
        lifecycle_counter: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct SpyArgs {
        value: String,
        parse_counter: Arc<AtomicUsize>,
    }

    impl<'de> Deserialize<'de> for SpyArgs {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct RawSpyArgs {
                value: String,
            }

            static PARSE_COUNT: AtomicUsize = AtomicUsize::new(0);

            let raw = RawSpyArgs::deserialize(deserializer)?;
            PARSE_COUNT.fetch_add(1, Ordering::SeqCst);

            Ok(Self {
                value: raw.value,
                parse_counter: Arc::new(AtomicUsize::new(PARSE_COUNT.load(Ordering::SeqCst))),
            })
        }
    }

    #[async_trait]
    impl TypedTool for SpyTool {
        type Args = SpyArgs;

        fn name(&self) -> &'static str {
            "spy_tool"
        }

        fn schema(&self) -> &'static str {
            "spy schema"
        }

        fn assess(&self, args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
            self.lifecycle_counter.fetch_add(1, Ordering::SeqCst);
            ToolCallAssessment {
                risk: RiskLevel::ReadOnlyWorkspace,
                policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
                reasons: vec![format!(
                    "assessed {} after {} parse(s)",
                    args.value,
                    args.parse_counter.load(Ordering::SeqCst)
                )],
            }
        }

        fn describe(&self, args: &Self::Args) -> String {
            self.lifecycle_counter.fetch_add(1, Ordering::SeqCst);
            format!(
                "described {} after {} parse(s)",
                args.value,
                args.parse_counter.load(Ordering::SeqCst)
            )
        }

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolCallResult {
            self.lifecycle_counter.fetch_add(1, Ordering::SeqCst);
            ToolCallResult::success(json!({
                "value": args.value,
                "parse_count": args.parse_counter.load(Ordering::SeqCst),
            }))
        }
    }

    fn prepare_spy_tool() -> (PreparedToolCall, Arc<AtomicUsize>) {
        let lifecycle_counter = Arc::new(AtomicUsize::new(0));
        let mut manager = ToolManager::new();
        manager.register_tool(SpyTool {
            lifecycle_counter: lifecycle_counter.clone(),
        });

        let prepared = manager
            .prepare_tool(
                &kraai_types::ToolId::new("spy_tool"),
                json!({ "value": "alpha" }),
            )
            .expect("prepare succeeds");

        (prepared, lifecycle_counter)
    }

    #[test]
    fn prepare_tool_returns_not_found_for_unknown_tool() {
        let manager = ToolManager::new();
        let Err(error) = manager.prepare_tool(&kraai_types::ToolId::new("missing"), json!({}))
        else {
            panic!("missing tool should fail");
        };

        match error {
            ToolError::ToolNotFound(tool_id) => assert_eq!(tool_id.as_str(), "missing"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn prepare_tool_returns_preparation_error_for_invalid_args() {
        let mut manager = ToolManager::new();
        manager.register_tool(SpyTool {
            lifecycle_counter: Arc::new(AtomicUsize::new(0)),
        });

        let Err(error) = manager.prepare_tool(&kraai_types::ToolId::new("spy_tool"), json!({}))
        else {
            panic!("invalid args should fail");
        };

        match error {
            ToolError::Preparation(message) => {
                assert!(message.contains("Unable to validate spy_tool arguments"));
                assert!(message.contains("value"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn prepared_tool_call_reuses_typed_args_across_lifecycle() {
        let (prepared, lifecycle_counter) = prepare_spy_tool();
        let config = ToolCallGlobalConfig {
            workspace_dir: PathBuf::from("/tmp/workspace"),
        };
        let snapshot = ToolStateSnapshot::default();
        let ctx = ToolContext {
            global_config: &config,
            tool_state_snapshot: &snapshot,
        };

        assert_eq!(prepared.tool_id().as_str(), "spy_tool");
        assert_eq!(prepared.args_json(), &json!({ "value": "alpha" }));
        assert_eq!(prepared.describe(), "described alpha after 1 parse(s)");
        assert_eq!(
            prepared.assess(&ctx).reasons,
            vec![String::from("assessed alpha after 1 parse(s)")]
        );

        match prepared.call(&ctx).await.output {
            ToolOutput::Success { data } => {
                assert_eq!(data, json!({ "value": "alpha", "parse_count": 1 }));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        assert_eq!(lifecycle_counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn read_text_path_rejects_missing_and_directory_paths() {
        let missing = Path::new("/tmp/tool-core-definitely-missing");
        let error = read_text_path(missing).expect_err("missing path should fail");
        assert!(error.contains("file does not exist"));

        let error = read_text_path(Path::new("/tmp")).expect_err("directory should fail");
        assert!(error.contains("path is a directory"));
    }
}
