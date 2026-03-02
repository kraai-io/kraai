use std::io::Read;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tool_core::{Tool, ToolContext, ToolOutput, normalize_tool_path};
use toon_schema::ToonSchema;
use types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

pub struct ReadFileTool {}

#[derive(Deserialize, ToonSchema, Serialize)]
#[toon_schema(
    name = "read_files",
    description = "Read files from the filesystem and return their contents with line numbers"
)]
struct ReadFileToolArgs {
    #[toon_schema(
        description = "List of file paths to read",
        example = "[\"/path/to/file.txt\", \"/path/to/another/file.md\"]",
        min = 1
    )]
    files: Vec<String>,
}

#[derive(Serialize)]
struct ReadFileToolOutput {
    files: Vec<String>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        ReadFileToolArgs::tool_name()
    }

    fn schema(&self) -> &'static str {
        ReadFileToolArgs::toon_schema()
    }

    fn assess(&self, args: &serde_json::Value, ctx: &ToolContext<'_>) -> ToolCallAssessment {
        let parsed: ReadFileToolArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolCallAssessment {
                    risk: RiskLevel::OutsideWorkspace,
                    policy: ExecutionPolicy::AlwaysAsk,
                    reasons: vec![format!("Unable to validate read_files arguments: {error}")],
                };
            }
        };

        let mut reasons = Vec::new();
        let mut risk = RiskLevel::ReadOnlyWorkspace;

        for file in &parsed.files {
            let normalized = normalize_tool_path(ctx.workspace_root, file);
            if normalized.starts_with(ctx.workspace_root) {
                reasons.push(format!("Reads workspace file {}", normalized.display()));
            } else {
                risk = RiskLevel::OutsideWorkspace;
                reasons.push(format!(
                    "Reads file outside workspace {}",
                    normalized.display()
                ));
            }
        }

        ToolCallAssessment {
            risk,
            policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
            reasons,
        }
    }

    async fn call(&self, args: serde_json::Value) -> ToolOutput {
        let args: ReadFileToolArgs = match serde_json::from_value(args) {
            Ok(x) => x,
            Err(e) => {
                return ToolOutput::error(format!("args error: {}", e));
            }
        };
        let mut files_out = vec![];
        for f in args.files {
            let mut file = match std::fs::File::open(&f) {
                Ok(f) => f,
                Err(e) => {
                    return ToolOutput::error(format!("unable to open file {}: {}", f, e));
                }
            };
            let mut str = String::new();
            match file.read_to_string(&mut str) {
                Ok(_) => {}
                Err(e) => return ToolOutput::error(format!("unable to read file {}: {}", f, e)),
            }
            let mut lined_str = String::new();
            for (i, l) in str.split('\n').enumerate() {
                lined_str.push_str(&format!("{}|{}\n", i, l));
            }
            files_out.push(lined_str);
        }
        let out = ReadFileToolOutput { files: files_out };
        ToolOutput::success(out)
    }

    async fn describe(&self, args: serde_json::Value) -> String {
        let args: ReadFileToolArgs =
            serde_json::from_value(args).unwrap_or(ReadFileToolArgs { files: vec![] });
        let count = args.files.len();
        let files_str = args.files.join(", ");
        format!("Read {} file(s): {}", count, files_str)
    }
}
