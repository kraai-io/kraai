use std::{collections::BTreeMap, path::Path};

use anyhow::Result;

use crate::{AgentConfig, tools::ToolTrait};

async fn read_file_tool(base_path: &str, path: &str) -> Result<String> {
    let mut ret = String::new();
    ret.push_str(&format!("read_file {}\n", path));
    let path = Path::new(base_path).join(path);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            ret.push_str("```\n");
            for (i, line) in content.lines().enumerate() {
                ret.push_str(&format!("{} | {}\n", i + 1, line));
            }
            ret.push_str("```");
        }
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => {
                ret.push_str("Unable to find file specified");
            }
            _ => {
                eprintln!("unknown error: {}", e);
                ret.push_str("Unknown error");
            }
        },
    }

    Ok(ret)
}

fn prompt() -> String {
    include_str!("prompts/read_file.md").to_string()
}

pub struct ReadFileTool;

impl ToolTrait for ReadFileTool {
    async fn call(params: BTreeMap<String, String>, config: &AgentConfig) -> Result<String> {
        let path = params.get("path").unwrap();
        let base_path = &config.base_path;
        read_file_tool(base_path, path).await
    }

    fn prompt(_config: &AgentConfig) -> String {
        prompt()
    }
}
