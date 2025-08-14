use std::{collections::BTreeMap, str::FromStr, vec};

use anyhow::Result;
use regex::Regex;

use crate::{AgentConfig, tools::read_file::ReadFileTool};

mod read_file;

const TOOL_PROMPT: &str = include_str!("prompts/tool.md");

#[derive(Debug, Clone)]
pub enum Tool {
    ReadFile,
}

impl std::str::FromStr for Tool {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read_file" => Ok(Tool::ReadFile),
            _ => Err(format!("unknown tool name: {}", s)),
        }
    }
}

pub async fn call_tool(
    config: &AgentConfig,
    tool: Tool,
    params: BTreeMap<String, String>,
) -> Result<String> {
    match tool {
        Tool::ReadFile => ReadFileTool::call(params, config).await,
    }
}

pub fn get_tool_prompts(config: &AgentConfig) -> String {
    let mut str = String::new();
    str.push_str(TOOL_PROMPT);

    for tool in get_available_tools(config) {
        str.push_str(&match tool {
            Tool::ReadFile => ReadFileTool::prompt(config),
        });
    }
    str
}

pub fn get_available_tools(_config: &AgentConfig) -> Vec<Tool> {
    let mut ret = vec![];

    ret.push(Tool::ReadFile);

    ret
}

pub fn parse_tool_calls(content: &str) -> Result<Vec<(Tool, BTreeMap<String, String>)>> {
    let re = Regex::new(r"(?s)```tool\n(.+?)\n```").unwrap();
    let mut calls = vec![];

    for caps in re.captures_iter(content) {
        let json_block = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
        let parsed_json: serde_json::Value = serde_json::from_str(json_block)?;

        if let Some(tool_name) = parsed_json["name"].as_str() {
            if let Ok(tool) = Tool::from_str(tool_name) {
                let mut params = BTreeMap::new();
                if let Some(parameters) = parsed_json["parameters"].as_object() {
                    for (key, value) in parameters {
                        params.insert(key.clone(), value.to_string().trim_matches('"').to_string());
                    }
                }
                calls.push((tool, params));
            }
        }
    }
    Ok(calls)
}

trait ToolTrait {
    async fn call(params: BTreeMap<String, String>, config: &AgentConfig) -> Result<String>;
    fn prompt(config: &AgentConfig) -> String;
}
