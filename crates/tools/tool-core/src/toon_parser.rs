use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use toon_format::{ToonError, decode_default};

static TOOL_CALL_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_call>\s*\n?(.*?)</tool_call>").expect("valid regex"));

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub tool_id: String,
    pub args: Value,
    pub raw_content: String,
}

#[derive(Debug, Clone)]
pub struct FailedToolCall {
    pub raw_content: String,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct ParseResult {
    pub successful: Vec<ParsedToolCall>,
    pub failed: Vec<FailedToolCall>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Failed to decode Toon: {0}")]
    ToonDecode(#[from] ToonError),
    #[error("Tool call body must decode to an object")]
    InvalidToolCallObject,
    #[error("Missing 'tool' field in tool call")]
    MissingToolField,
    #[error("Tool field must be a string")]
    InvalidToolField,
}

pub fn parse_tool_calls(text: &str) -> ParseResult {
    let mut successful = Vec::new();
    let mut failed = Vec::new();

    for raw in extract_tool_call_blocks(text) {
        match parse_single_tool_call(&raw) {
            Ok(parsed) => successful.push(parsed),
            Err(e) => failed.push(FailedToolCall {
                raw_content: raw,
                error: e.to_string(),
            }),
        }
    }

    ParseResult { successful, failed }
}

fn extract_tool_call_blocks(text: &str) -> Vec<String> {
    TOOL_CALL_BLOCK_RE
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).map(|content| content.as_str().to_string()))
        .collect()
}

fn parse_single_tool_call(toon_content: &str) -> Result<ParsedToolCall, ParseError> {
    let value: Value = decode_default(toon_content)?;

    let obj = value.as_object().ok_or(ParseError::InvalidToolCallObject)?;

    let tool_value = obj.get("tool").ok_or(ParseError::MissingToolField)?;

    let tool_id = tool_value
        .as_str()
        .ok_or(ParseError::InvalidToolField)?
        .to_string();

    let mut args = serde_json::Map::new();
    for (k, v) in obj {
        if k != "tool" {
            args.insert(k.clone(), v.clone());
        }
    }

    Ok(ParsedToolCall {
        tool_id,
        args: Value::Object(args),
        raw_content: toon_content.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_tool_call() {
        let text = r#"<tool_call>
tool: read_files
files[2]: /etc/passwd,/etc/hosts
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "read_files");
    }

    #[test]
    fn test_parse_multiple_tool_calls() {
        let text = r#"<tool_call>
tool: read_files
files[1]: /etc/passwd
</tool_call>

Some text in between.

<tool_call>
tool: another_tool
arg: value
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 2);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "read_files");
        assert_eq!(result.successful[1].tool_id, "another_tool");
    }

    #[test]
    fn test_no_tool_calls() {
        let text = "Just regular text without any tool calls.";
        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert!(result.failed.is_empty());
    }

    #[test]
    fn test_missing_tool_field() {
        let text = r#"<tool_call>
files[1]: /etc/passwd
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(result.failed[0].error.contains("tool"));
    }

    #[test]
    fn test_parse_with_args() {
        let text = r#"<tool_call>
tool: read_files
files[2]: /etc/passwd,/etc/hosts
encoding: utf-8
max_size: 1048576
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "read_files");
        assert!(result.successful[0].args.get("files").is_some());
        assert!(result.successful[0].args.get("encoding").is_some());
        assert!(result.successful[0].args.get("max_size").is_some());
        assert!(result.successful[0].args.get("tool").is_none());
    }

    #[test]
    fn test_parse_real_format() {
        let text = r#"Some response text.

<tool_call>
tool: read_files
files[2]: /etc/passwd,/etc/hosts
encoding: utf-8
max_size: 1048576
</tool_call>

More text after."#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "read_files");
    }

    #[test]
    fn test_mixed_valid_and_invalid() {
        let text = r#"<tool_call>
tool: valid_tool
arg: value
</tool_call>

<tool_call>
invalid_field: value
</tool_call>

<tool_call>
tool: another_valid
x: 1
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 2);
        assert_eq!(result.failed.len(), 1);
    }

    #[test]
    fn test_malformed_toon_is_reported() {
        let text = r#"<tool_call>
tool read_files
files[1]: /etc/passwd
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(result.failed[0].error.contains("Failed to decode Toon"));
    }

    #[test]
    fn test_non_object_tool_call_is_reported() {
        let text = r#"<tool_call>
[3]: one,two,three
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].raw_content, "[3]: one,two,three\n");
        assert!(result.failed[0].error.contains("decode to an object"));
    }

    #[test]
    fn test_numeric_tool_field_is_invalid() {
        let text = r#"<tool_call>
tool: 123
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(
            result.failed[0]
                .error
                .contains("Tool field must be a string")
        );
    }

    #[test]
    fn test_object_tool_field_is_invalid() {
        let text = r#"<tool_call>
tool:
  nested: value
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(
            result.failed[0]
                .error
                .contains("Tool field must be a string")
        );
    }

    #[test]
    fn test_empty_tool_call_block_is_reported() {
        let text = "<tool_call>\n</tool_call>";

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].raw_content, "");
    }
}
