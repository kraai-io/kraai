use super::boxed_error as command_error;
use base64::Engine;
use kraai_command_core::CommandContext;
use nu_protocol::{ShellError, Span};
use serde_json::{Value, json};

pub(super) fn prepare(
    mut result: Value,
    context: &CommandContext,
    span: Span,
) -> Result<Value, Box<ShellError>> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let message = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|content| content.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(command_error(
            "MCP tool failed",
            if message.is_empty() {
                result.to_string()
            } else {
                message
            },
            span,
        ));
    }
    if let Some(contents) = result.get_mut("content").and_then(Value::as_array_mut) {
        for content in contents {
            if content.get("type").and_then(Value::as_str) != Some("image") {
                continue;
            }
            let data = content
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| command_error("Invalid MCP image", "Missing image data", span))?;
            if data.len() > kraai_types::image::MAX_IMAGE_BYTES.div_ceil(3) * 4 {
                return Err(command_error(
                    "Invalid MCP image",
                    "Image exceeds attachment byte limit",
                    span,
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|error| command_error("Invalid MCP image", error.to_string(), span))?;
            let attachment = context
                .images()
                .attach(bytes)
                .map_err(|error| command_error("MCP image attachment failed", error, span))?;
            *content = json!({"type": "image", "attachment": attachment});
        }
    }
    Ok(result)
}
