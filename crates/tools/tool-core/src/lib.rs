#![forbid(unsafe_code)]

pub mod toon_parser;

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use types::{
    ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolCallGlobalConfig, ToolId, ToolStateDelta,
};

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Tool not found: {0}")]
    ToolNotFound(ToolId),
    #[error("{0}")]
    Preparation(String),
    #[error("Failed to serialize tool output: {0}")]
    OutputSerialization(#[from] serde_json::Error),
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Success {
        #[serde(flatten)]
        data: serde_json::Value,
    },
    Error {
        message: String,
    },
}

impl ToolOutput {
    pub fn error(message: String) -> Self {
        Self::Error { message }
    }

    pub fn success<D: Serialize>(data: D) -> Self {
        match serde_json::to_value(data) {
            Ok(data) => Self::Success { data },
            Err(error) => Self::error(format!("failed to serialize tool output: {error}")),
        }
    }
}

pub struct ToolContext<'a> {
    pub global_config: &'a ToolCallGlobalConfig,
}

#[async_trait]
pub trait TypedTool: Send + Sync + Clone + 'static {
    type Args: DeserializeOwned + Send + Sync + Clone + 'static;

    fn name(&self) -> &'static str;

    fn schema(&self) -> &'static str;

    fn assess(&self, _args: &Self::Args, _ctx: &ToolContext<'_>) -> ToolCallAssessment {
        ToolCallAssessment {
            risk: RiskLevel::WriteOutsideWorkspace,
            policy: ExecutionPolicy::AlwaysAsk,
            reasons: vec![String::from(
                "Tool does not define a custom autonomy policy",
            )],
        }
    }

    fn describe(&self, _args: &Self::Args) -> String {
        format!("{}: <typed args>", self.name())
    }

    fn successful_tool_state_deltas(
        &self,
        _args: &Self::Args,
        _ctx: &ToolContext<'_>,
    ) -> Vec<ToolStateDelta> {
        Vec::new()
    }

    async fn call(&self, args: Self::Args, ctx: &ToolContext<'_>) -> ToolOutput;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolPath {
    path: PathBuf,
    within_workspace: bool,
}

impl ResolvedToolPath {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_within_workspace(&self) -> bool {
        self.within_workspace
    }
}

#[async_trait]
trait PreparedToolCallInner: Send + Sync {
    fn assess(&self, ctx: &ToolContext<'_>) -> ToolCallAssessment;

    fn describe(&self) -> String;

    fn successful_tool_state_deltas(&self, ctx: &ToolContext<'_>) -> Vec<ToolStateDelta>;

    async fn call(&self, ctx: &ToolContext<'_>) -> ToolOutput;
}

struct TypedPreparedToolCall<T: TypedTool> {
    tool: T,
    args: T::Args,
}

#[async_trait]
impl<T: TypedTool> PreparedToolCallInner for TypedPreparedToolCall<T> {
    fn assess(&self, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        self.tool.assess(&self.args, ctx)
    }

    fn describe(&self) -> String {
        self.tool.describe(&self.args)
    }

    fn successful_tool_state_deltas(&self, ctx: &ToolContext<'_>) -> Vec<ToolStateDelta> {
        self.tool.successful_tool_state_deltas(&self.args, ctx)
    }

    async fn call(&self, ctx: &ToolContext<'_>) -> ToolOutput {
        self.tool.call(self.args.clone(), ctx).await
    }
}

#[derive(Clone)]
pub struct PreparedToolCall {
    tool_id: ToolId,
    args_json: serde_json::Value,
    inner: Arc<dyn PreparedToolCallInner>,
}

impl PreparedToolCall {
    pub fn tool_id(&self) -> &ToolId {
        &self.tool_id
    }

    pub fn args_json(&self) -> &serde_json::Value {
        &self.args_json
    }

    pub fn assess(&self, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        self.inner.assess(ctx)
    }

    pub fn describe(&self) -> String {
        self.inner.describe()
    }

    pub fn successful_tool_state_deltas(&self, ctx: &ToolContext<'_>) -> Vec<ToolStateDelta> {
        self.inner.successful_tool_state_deltas(ctx)
    }

    pub async fn call(&self, ctx: &ToolContext<'_>) -> ToolOutput {
        self.inner.call(ctx).await
    }
}

trait ErasedTool: Send + Sync {
    fn schema(&self) -> &'static str;

    fn prepare(
        &self,
        tool_id: &ToolId,
        args: serde_json::Value,
    ) -> Result<PreparedToolCall, ToolError>;
}

struct TypedToolAdapter<T: TypedTool> {
    tool: T,
}

impl<T: TypedTool> TypedToolAdapter<T> {
    fn new(tool: T) -> Self {
        Self { tool }
    }
}

impl<T: TypedTool> ErasedTool for TypedToolAdapter<T> {
    fn schema(&self) -> &'static str {
        self.tool.schema()
    }

    fn prepare(
        &self,
        tool_id: &ToolId,
        args: serde_json::Value,
    ) -> Result<PreparedToolCall, ToolError> {
        let parsed = serde_json::from_value::<T::Args>(args.clone()).map_err(|error| {
            ToolError::Preparation(format!(
                "Unable to validate {} arguments: {error}",
                self.tool.name()
            ))
        })?;

        Ok(PreparedToolCall {
            tool_id: tool_id.clone(),
            args_json: args,
            inner: Arc::new(TypedPreparedToolCall {
                tool: self.tool.clone(),
                args: parsed,
            }),
        })
    }
}

#[derive(Default, Clone)]
pub struct ToolManager {
    tools: BTreeMap<ToolId, Arc<dyn ErasedTool>>,
}

impl ToolManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_tool<T>(&mut self, tool: T)
    where
        T: TypedTool,
    {
        let id = ToolId::new(tool.name());
        self.tools.insert(id, Arc::new(TypedToolAdapter::new(tool)));
    }

    pub fn has_tool(&self, id: &ToolId) -> bool {
        self.tools.contains_key(id)
    }

    pub fn list_tools(&self) -> Vec<ToolId> {
        self.tools.keys().cloned().collect()
    }

    pub fn generate_system_prompt(&self) -> String {
        self.tools
            .values()
            .map(|t| t.schema())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn generate_system_prompt_for_tools(
        &self,
        tool_ids: &[ToolId],
    ) -> Result<String, ToolError> {
        let mut sections = Vec::with_capacity(tool_ids.len());
        for tool_id in tool_ids {
            let tool = self
                .tools
                .get(tool_id)
                .ok_or_else(|| ToolError::ToolNotFound(tool_id.clone()))?;
            sections.push(tool.schema());
        }
        Ok(sections.join("\n\n"))
    }

    pub fn prepare_tool(
        &self,
        id: &ToolId,
        args: serde_json::Value,
    ) -> Result<PreparedToolCall, ToolError> {
        let tool = self
            .tools
            .get(id)
            .ok_or_else(|| ToolError::ToolNotFound(id.clone()))?;
        tool.prepare(id, args)
    }
}

pub fn normalize_tool_path(workspace_root: &Path, raw_path: &str) -> PathBuf {
    let path = Path::new(raw_path);
    let is_absolute = path.is_absolute();
    let base = if is_absolute {
        PathBuf::new()
    } else {
        workspace_root.to_path_buf()
    };

    let mut normalized = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.parent().is_some() {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    if is_absolute && !normalized.is_absolute() {
        normalized = Path::new("/").join(normalized);
    }

    normalized
}

pub fn resolve_tool_path(workspace_root: &Path, raw_path: &str) -> ResolvedToolPath {
    let path = normalize_tool_path(workspace_root, raw_path);
    let within_workspace = path.starts_with(workspace_root);
    ResolvedToolPath {
        path,
        within_workspace,
    }
}

pub fn assess_read_path(
    workspace_root: &Path,
    raw_path: &str,
    workspace_reason_prefix: &str,
    outside_reason_prefix: &str,
) -> ToolCallAssessment {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    let reason = if resolved.is_within_workspace() {
        format!("{} {}", workspace_reason_prefix, resolved.path().display())
    } else {
        format!("{} {}", outside_reason_prefix, resolved.path().display())
    };

    ToolCallAssessment {
        risk: if resolved.is_within_workspace() {
            RiskLevel::ReadOnlyWorkspace
        } else {
            RiskLevel::ReadOnlyOutsideWorkspace
        },
        policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
        reasons: vec![reason],
    }
}

pub fn assess_write_path(
    workspace_root: &Path,
    raw_path: &str,
    workspace_reason_prefix: &str,
    outside_reason_prefix: &str,
) -> ToolCallAssessment {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    let reason = if resolved.is_within_workspace() {
        format!("{} {}", workspace_reason_prefix, resolved.path().display())
    } else {
        format!("{} {}", outside_reason_prefix, resolved.path().display())
    };

    ToolCallAssessment {
        risk: if resolved.is_within_workspace() {
            RiskLevel::UndoableWorkspaceWrite
        } else {
            RiskLevel::WriteOutsideWorkspace
        },
        policy: ExecutionPolicy::AlwaysAsk,
        reasons: vec![reason],
    }
}

pub fn format_text_with_line_numbers(contents: &str) -> String {
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}|{}", index + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use serde::ser::{Error as _, Serialize, Serializer};
    use serde::{Deserialize, Deserializer};
    use serde_json::json;
    use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment, ToolCallGlobalConfig};

    use super::{
        PreparedToolCall, ToolContext, ToolError, ToolManager, ToolOutput, TypedTool,
        assess_read_path, assess_write_path, format_text_with_line_numbers, resolve_tool_path,
    };

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
        let output = ToolOutput::success(FailingSerialize);

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("failed to serialize tool output"));
                assert!(message.contains("intentional failure"));
            }
            ToolOutput::Success { .. } => panic!("expected tool serialization failure"),
        }
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

        async fn call(&self, args: Self::Args, _ctx: &ToolContext<'_>) -> ToolOutput {
            self.lifecycle_counter.fetch_add(1, Ordering::SeqCst);
            ToolOutput::success(json!({
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
            .prepare_tool(&types::ToolId::new("spy_tool"), json!({ "value": "alpha" }))
            .expect("prepare succeeds");

        (prepared, lifecycle_counter)
    }

    #[test]
    fn prepare_tool_returns_not_found_for_unknown_tool() {
        let manager = ToolManager::new();
        let error = match manager.prepare_tool(&types::ToolId::new("missing"), json!({})) {
            Ok(_) => panic!("missing tool should fail"),
            Err(error) => error,
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

        let error = match manager.prepare_tool(&types::ToolId::new("spy_tool"), json!({})) {
            Ok(_) => panic!("invalid args should fail"),
            Err(error) => error,
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
        let ctx = ToolContext {
            global_config: &config,
        };

        assert_eq!(prepared.tool_id().as_str(), "spy_tool");
        assert_eq!(prepared.args_json(), &json!({ "value": "alpha" }));
        assert_eq!(prepared.describe(), "described alpha after 1 parse(s)");
        assert_eq!(
            prepared.assess(&ctx).reasons,
            vec![String::from("assessed alpha after 1 parse(s)")]
        );

        match prepared.call(&ctx).await {
            ToolOutput::Success { data } => {
                assert_eq!(data, json!({ "value": "alpha", "parse_count": 1 }));
            }
            ToolOutput::Error { message } => panic!("unexpected error: {message}"),
        }

        assert_eq!(lifecycle_counter.load(Ordering::SeqCst), 3);
    }
}
