use kraai_execution_types::WebSearchResponse;
use serde_json::Value;

pub(super) fn decode(body: &str, max_chars: usize) -> Result<WebSearchResponse, String> {
    let body = body.trim_prefix('\u{feff}');
    if body.trim_start().starts_with('{') {
        return decode_message(body, max_chars)?.ok_or_else(missing_result);
    }
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    for event in normalized.split("\n\n") {
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(|data| data.trim_prefix(' ')))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Some(response) = decode_message(&data, max_chars)? {
            return Ok(response);
        }
    }
    Err(missing_result())
}

fn missing_result() -> String {
    String::from("malformed web search response: missing result")
}

fn decode_message(body: &str, max_chars: usize) -> Result<Option<WebSearchResponse>, String> {
    let message: Value = serde_json::from_str(body)
        .map_err(|error| format!("malformed web search response: {error}"))?;
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(String::from(
            "malformed web search response: invalid JSON-RPC version",
        ));
    }
    if message.get("id").is_none() && message.get("method").and_then(Value::as_str).is_some() {
        return Ok(None);
    }
    if message.get("id").and_then(Value::as_u64) != Some(1) {
        return Err(String::from(
            "malformed web search response: unexpected request id",
        ));
    }
    if let Some(error) = message.get("error") {
        return Err(format!(
            "web search RPC error: {}",
            error.to_string().chars().take(512).collect::<String>()
        ));
    }
    let result = message.get("result").ok_or_else(missing_result)?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(String::from("web search provider returned a tool error"));
    }
    let blocks = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(missing_result)?;
    let mut texts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            texts.push(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(missing_result)?,
            );
        }
    }
    if !blocks.is_empty() && texts.is_empty() {
        return Err(String::from("web search response contains no text blocks"));
    }
    let content = texts.join("\n\n");
    let end = content
        .char_indices()
        .nth(max_chars)
        .map(|(offset, _)| offset);
    let truncated = end.is_some();
    let mut content = content;
    if let Some(end) = end {
        content.truncate(end);
    }
    Ok(Some(WebSearchResponse {
        provider: String::from("exa"),
        content,
        truncated,
    }))
}

#[cfg(test)]
mod tests;
