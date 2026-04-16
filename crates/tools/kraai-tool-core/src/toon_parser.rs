use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use toon_format::{ToonError, decode_default};

static TOOL_CALL_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<tool_call>\s*\n?(.*?)</tool_call>").expect("valid regex"));
static THINK_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)</?(?:think|thinking)\b[^>]*>").expect("valid think tag regex")
});

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub tool_id: String,
    pub args: Value,
    pub raw_content: String,
}

#[derive(Debug, Clone)]
pub struct ParseFailure {
    pub kind: ParseFailureKind,
    pub raw_content: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseFailureKind {
    ToolCall,
    ThinkingBlock,
}

#[derive(Debug, Clone)]
pub struct ParseResult {
    pub successful: Vec<ParsedToolCall>,
    pub failed: Vec<ParseFailure>,
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
    let (visible_text, mut failed) = strip_thinking_blocks(text);
    let mut successful = Vec::new();

    for raw in extract_tool_call_blocks(&visible_text) {
        match parse_single_tool_call(&raw) {
            Ok(parsed) => successful.push(parsed),
            Err(e) => failed.push(ParseFailure {
                kind: ParseFailureKind::ToolCall,
                raw_content: raw,
                error: e.to_string(),
            }),
        }
    }

    ParseResult { successful, failed }
}

fn strip_thinking_blocks(text: &str) -> (String, Vec<ParseFailure>) {
    let mut visible = String::new();
    let mut failures = Vec::new();
    let mut cursor = 0usize;
    let mut thinking_start = None;

    for matched in THINK_TAG_RE.find_iter(text) {
        let tag = matched.as_str();
        let is_closing = tag.starts_with("</");

        match (thinking_start, is_closing) {
            (None, false) => {
                visible.push_str(&text[cursor..matched.start()]);
                thinking_start = Some(matched.start());
                cursor = matched.end();
            }
            (Some(_), true) => {
                thinking_start = None;
                cursor = matched.end();
            }
            (None, true) | (Some(_), false) => {}
        }
    }

    if let Some(start) = thinking_start {
        failures.push(ParseFailure {
            kind: ParseFailureKind::ThinkingBlock,
            raw_content: text[start..].to_string(),
            error: String::from("Missing closing </think> or </thinking> tag"),
        });
        return (visible, failures);
    }

    visible.push_str(&text[cursor..]);
    (visible, failures)
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

    #[test]
    fn test_tool_call_inside_think_is_ignored() {
        let text = r#"<think>
<tool_call>
tool: hidden_tool
</tool_call>
</think>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert!(result.failed.is_empty());
    }

    #[test]
    fn test_tool_call_inside_thinking_with_attributes_is_ignored() {
        let text = r#"<thinking class="chain">
<tool_call>
tool: hidden_tool
</tool_call>
</thinking>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert!(result.failed.is_empty());
    }

    #[test]
    fn test_tool_calls_before_and_after_thinking_are_detected() {
        let text = r#"<tool_call>
tool: before_tool
</tool_call>
<think>
<tool_call>
tool: hidden_tool
</tool_call>
</think>
<tool_call>
tool: after_tool
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 2);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "before_tool");
        assert_eq!(result.successful[1].tool_id, "after_tool");
    }

    #[test]
    fn test_mixed_visible_and_thinking_tool_calls_only_returns_visible_call() {
        let text = r#"before
<tool_call>
tool: visible_tool
</tool_call>
<think>
<tool_call>
tool: hidden_tool
</tool_call>
</think>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "visible_tool");
    }

    #[test]
    fn test_unclosed_think_reports_thinking_block_failure() {
        let text = r#"<think>
<tool_call>
tool: hidden_tool
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].kind, ParseFailureKind::ThinkingBlock);
        assert!(
            result.failed[0]
                .error
                .contains("Missing closing </think> or </thinking> tag")
        );
    }

    #[test]
    fn test_stray_closing_think_does_not_suppress_visible_tool_calls() {
        let text = r#"</think>
<tool_call>
tool: visible_tool
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "visible_tool");
    }

    #[test]
    fn test_case_insensitive_thinking_tags_are_handled() {
        let text = r#"<THINK>
<tool_call>
tool: hidden_tool
</tool_call>
</THINK>
<tool_call>
tool: visible_tool
</tool_call>"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "visible_tool");
    }
}
