use agent_client_protocol::{Result, schema::v1 as acp};
use base64::Engine;
use kraai_runtime::RuntimeHandle;
use kraai_types::{ContentPart, MessageContent};

use crate::error;

pub(crate) async fn prompt(
    runtime: &RuntimeHandle,
    blocks: Vec<acp::ContentBlock>,
) -> Result<MessageContent> {
    let mut parts = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block {
            acp::ContentBlock::Text(text) => parts.push(ContentPart::Text { text: text.text }),
            acp::ContentBlock::ResourceLink(link) => parts.push(ContentPart::Text {
                text: format!(
                    "Resource: {}\nURI: {}\n{}",
                    link.name,
                    link.uri,
                    link.description.unwrap_or_default()
                ),
            }),
            acp::ContentBlock::Image(image) => {
                if image.data.len()
                    > kraai_types::image::MAX_IMAGE_BYTES
                        .saturating_mul(4)
                        .div_ceil(3)
                        + 4
                {
                    return Err(error::invalid("Image exceeds the attachment size limit"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(image.data)
                    .map_err(|error| crate::error::invalid(error.to_string()))?;
                let image = runtime.import_image(bytes).await.map_err(error::runtime)?;
                parts.push(ContentPart::Image { image });
            }
            _ => return Err(error::invalid("Unsupported prompt content type")),
        }
    }
    if parts.is_empty() {
        return Err(error::invalid("Prompt must contain content"));
    }
    Ok(MessageContent(parts))
}

pub(crate) fn text(value: impl Into<String>) -> acp::ContentBlock {
    acp::ContentBlock::Text(acp::TextContent::new(value))
}

pub(crate) async fn blocks(
    runtime: &RuntimeHandle,
    content: &MessageContent,
) -> Result<Vec<acp::ContentBlock>> {
    let mut blocks = Vec::with_capacity(content.parts().len());
    for part in content.parts() {
        blocks.push(match part {
            ContentPart::Text { text: value } => text(value.clone()),
            ContentPart::Image { image } => {
                let bytes = runtime
                    .read_image(image.clone())
                    .await
                    .map_err(error::runtime)?;
                acp::ContentBlock::Image(acp::ImageContent::new(
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                    image.mime_type.clone(),
                ))
            }
        });
    }
    Ok(blocks)
}

pub(crate) async fn tool_result(
    runtime: &RuntimeHandle,
    call_id: String,
    output: &MessageContent,
) -> Result<acp::SessionUpdate> {
    let rendered = output.text_only();
    let header = rendered
        .split_once('>')
        .map(|(header, _)| header)
        .unwrap_or_default();
    let succeeded = header.starts_with("<tool_call_result status=\"completed\"")
        && (!header.contains(" exit_code=") || header.contains(" exit_code=\"0\""));
    let status = if succeeded {
        acp::ToolCallStatus::Completed
    } else {
        acp::ToolCallStatus::Failed
    };
    Ok(acp::SessionUpdate::ToolCallUpdate(
        acp::ToolCallUpdate::new(
            call_id,
            acp::ToolCallUpdateFields::new().status(status).content(
                blocks(runtime, output)
                    .await?
                    .into_iter()
                    .map(|block| acp::ToolCallContent::Content(acp::Content::new(block)))
                    .collect::<Vec<_>>(),
            ),
        ),
    ))
}
