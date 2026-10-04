#![expect(
    clippy::panic_in_result_fn,
    reason = "protocol integration tests assert after fallible I/O"
)]

use crate::support::{Harness, Reply};
use base64::Engine;
use color_eyre::eyre::{Result, eyre};
use serde_json::{Value, json};

#[tokio::test]
async fn prompt_and_tool_images_survive_reload_without_source_file() -> Result<()> {
    let mut harness = Harness::new(vec![
        Reply::Script(String::from(
            "# timeout=10sec permissions=no-sandbox\nkraai-view-image image.png",
        )),
        "Viewed image.".into(),
    ])
    .await?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Png)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes.get_ref());
    tokio::fs::write(harness.root.path().join("image.png"), bytes.get_ref()).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness.send(json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{
        "sessionId":session,
        "prompt":[{"type":"text","text":"View the image"},{"type":"image","data":encoded,"mimeType":"image/png"}]
    }})).await?;
    let pending = harness
        .until(|value| value.get("method") == Some(&json!("session/request_permission")))
        .await?;
    let approval = pending.last().ok_or_else(|| eyre!("missing approval"))?;
    harness.send(json!({"jsonrpc":"2.0","id":approval.get("id"),"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})).await?;
    let live = harness
        .until(|value| value.get("id") == Some(&json!(2)))
        .await?;
    assert!(has_tool_image(&live, &encoded));
    tokio::fs::remove_file(harness.root.path().join("image.png")).await?;
    harness.restart().await?;
    harness.initialize().await?;
    let replay = harness
        .request(
            3,
            "session/load",
            json!({"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}),
        )
        .await?;
    assert!(
        replay
            .last()
            .is_some_and(|value| value.get("result").is_some())
    );
    assert!(replay.iter().any(|value| {
        value.pointer("/params/update/sessionUpdate") == Some(&json!("user_message_chunk"))
            && value.pointer("/params/update/content")
                == Some(&json!({"type":"image","data":encoded,"mimeType":"image/png"}))
    }));
    assert!(has_tool_image(&replay, &encoded));
    harness.stop().await
}

fn has_tool_image(messages: &[Value], encoded: &str) -> bool {
    messages.iter().any(|message| {
        message.pointer("/params/update/sessionUpdate") == Some(&json!("tool_call_update"))
            && message
                .pointer("/params/update/content")
                .and_then(Value::as_array)
                .is_some_and(|content| {
                    content.iter().any(|block| {
                        block.get("content")
                            == Some(&json!({"type":"image","data":encoded,"mimeType":"image/png"}))
                    })
                })
    })
}
