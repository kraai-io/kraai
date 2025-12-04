use std::{io::Read, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tool_core::{Tool, ToolOutput};
use toon_format::EncodeOptions;

pub struct ReadFileTool {}

#[derive(Deserialize)]
struct ReadFileToolArgs {
    files: Vec<String>,
}

#[derive(Serialize)]
struct ReadFileToolOutput {
    files: Vec<String>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> Arc<String> {
        "read_files".to_string().into()
    }

    fn description(&self) -> Arc<String> {
        "read the contents of files".to_string().into()
    }

    fn parameters_schema(&self) -> Arc<String> {
        let json = serde_json::json!({
            "tool": self.name(),
            "files": "Vec<RelativePath>"
        });
        let toon = toon_format::encode_object(json, &EncodeOptions::new()).unwrap();
        toon.into()
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
