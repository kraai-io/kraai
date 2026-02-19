use std::io::Read;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tool_core::{Tool, ToolOutput};
use toon_schema::ToonSchema;

pub struct ReadFileTool {}

#[derive(Deserialize, ToonSchema, Serialize)]
#[toon_schema(
    name = "read_files",
    description = "Read files from the filesystem and return their contents with line numbers"
)]
struct ReadFileToolArgs {
    #[toon_schema(
        description = "List of file paths to read",
        example = "[\"/path/to/file.txt\"]"
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
}
