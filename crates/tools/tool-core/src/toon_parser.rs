use regex::Regex;
use serde_json::Value;
use toon_format::{ToonError, decode_default};

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub tool_id: String,
    pub args: Value,
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
    #[error("Missing 'tool' field in tool call")]
    MissingToolField,
    #[error("Tool field must be a string")]
    InvalidToolField,
}

pub fn parse_tool_calls(text: &str) -> ParseResult {
    let re = Regex::new(r"```tool_call\s*\n([\s\S]*?)```").unwrap();
    let mut successful = Vec::new();
    let mut failed = Vec::new();

    for caps in re.captures_iter(text) {
        if let Some(toon_content) = caps.get(1) {
            let raw = toon_content.as_str().to_string();
            match parse_single_tool_call(&raw) {
                Ok(parsed) => successful.push(parsed),
                Err(e) => failed.push(FailedToolCall {
                    raw_content: raw,
                    error: e.to_string(),
                }),
            }
        }
    }

    ParseResult { successful, failed }
}

fn parse_single_tool_call(toon_content: &str) -> Result<ParsedToolCall, ParseError> {
    let value = decode_default(toon_content)?;

    let obj = match value {
        Value::Object(ref obj) => obj,
        _ => return Err(ParseError::MissingToolField),
    };

    let tool_id = obj
        .get("tool")
        .and_then(|v| v.as_str())
        .ok_or(ParseError::MissingToolField)?
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_tool_call() {
        let text = r#"```tool_call
tool: read_files
files[2]: /etc/passwd,/etc/hosts
```"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "read_files");
    }

    #[test]
    fn test_parse_multiple_tool_calls() {
        let text = r#"```tool_call
tool: read_files
files[1]: /etc/passwd
```

Some text in between.

```tool_call
tool: another_tool
arg: value
```"#;

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
        let text = r#"```tool_call
files[1]: /etc/passwd
```"#;

        let result = parse_tool_calls(text);
        assert!(result.successful.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert!(result.failed[0].error.contains("tool"));
    }

    #[test]
    fn test_parse_with_args() {
        let text = r#"```tool_call
tool: read_files
files[2]: /etc/passwd,/etc/hosts
encoding: utf-8
max_size: 1048576
```"#;

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

```tool_call
tool: read_files
files[2]: /etc/passwd,/etc/hosts
encoding: utf-8
max_size: 1048576
```

More text after."#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 1);
        assert!(result.failed.is_empty());
        assert_eq!(result.successful[0].tool_id, "read_files");
    }

    #[test]
    fn test_mixed_valid_and_invalid() {
        let text = r#"```tool_call
tool: valid_tool
arg: value
```

```tool_call
invalid_field: value
```

```tool_call
tool: another_valid
x: 1
```"#;

        let result = parse_tool_calls(text);
        assert_eq!(result.successful.len(), 2);
        assert_eq!(result.failed.len(), 1);
    }
}
