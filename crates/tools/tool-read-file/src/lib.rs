use std::{io::Read, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tool_core::{Tool, ToolOutput};
use toon_schema::ToonSchema;
use types::ToolId;

pub struct ReadFileTool {}

#[derive(Deserialize, ToonSchema, Serialize)]
#[toon_schema(name = "read_files")]
struct ReadFileToolArgs {
    #[toon_schema(example = "[\"/path/to/file.txt\", \"/path/to/other/file.ext\"]")]
    files: Vec<String>,
}

#[derive(Serialize)]
struct ReadFileToolOutput {
    files: Vec<String>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> ToolId {
        todo!()
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
            // TODO need to make sure it is relative to the workspace (this will require a config to be passed to this function)
            // std::fs::canonicalize(&f);

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
            let lines = str.split('\n');
            for (i, l) in lines.enumerate() {
                lined_str.push_str(&format!("{}|{}", i, l));
            }
            files_out.push(lined_str);
        }
        let out = ReadFileToolOutput { files: files_out };
        ToolOutput::success(out)
    }
}
