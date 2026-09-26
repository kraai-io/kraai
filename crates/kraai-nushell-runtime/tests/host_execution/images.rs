#![expect(
    clippy::panic_in_result_fn,
    reason = "integration tests assert after fallible host execution"
)]
use super::*;
use kraai_nushell_runtime::ImageAttachmentHandler;
use kraai_types::ImageAttachment;

#[derive(Default)]
struct RecordingImages(Mutex<Vec<Vec<u8>>>);

impl ImageAttachmentHandler for RecordingImages {
    fn attach_existing(
        &self,
        _sequence: u64,
        id: String,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>> {
        Box::pin(async move {
            Ok(ImageAttachment {
                id,
                mime_type: String::from("image/png"),
                width: 1,
                height: 1,
                byte_length: 5,
            })
        })
    }

    fn attach(
        &self,
        _sequence: u64,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>> {
        Box::pin(async move {
            let byte_length = bytes.len() as u64;
            self.0
                .lock()
                .map_err(|error| error.to_string())?
                .push(bytes);
            Ok(ImageAttachment {
                id: "a".repeat(64),
                mime_type: String::from("image/png"),
                width: 1,
                height: 1,
                byte_length,
            })
        })
    }
}

#[tokio::test]
async fn view_image_attaches_bytes_without_state_effects_or_binary_stdout()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = TestWorkspace::new();
    let bytes = [0, 255, 128, 10, 13];
    std::fs::write(workspace.0.join("image.png"), bytes)?;
    let images = Arc::new(RecordingImages::default());
    let effects = Arc::new(RecordingEffects::default());
    let mut plan = plan(
        b"kraai-view-image image.png | to json -r".to_vec(),
        &workspace,
    );
    plan.active_commands.push(String::from("kraai-view-image"));
    plan.image_attachment_handler = images.clone();
    plan.state_effect_handler = effects.clone();
    let result = execute(plan, CancellationToken::new()).await?;
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    let output: serde_json::Value = serde_json::from_slice(&result.output.stdout)?;
    assert_eq!(
        output.get("id").and_then(serde_json::Value::as_str),
        Some("a".repeat(64).as_str())
    );
    assert_eq!(
        output.get("width").and_then(serde_json::Value::as_u64),
        Some(1)
    );
    assert_eq!(output.as_object().map(|value| value.len()), Some(4));
    assert_eq!(
        *images.0.lock().map_err(|error| error.to_string())?,
        vec![bytes.to_vec()]
    );
    assert!(
        effects
            .requests
            .lock()
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn view_image_reopens_by_attachment_without_a_filesystem_path()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let workspace = TestWorkspace::new();
    let images = Arc::new(RecordingImages::default());
    let id = "a".repeat(64);
    let mut plan = plan(
        format!("kraai-view-image --attachment {id} | to json -r").into_bytes(),
        &workspace,
    );
    plan.active_commands.push(String::from("kraai-view-image"));
    plan.image_attachment_handler = images.clone();
    let result = execute(plan, CancellationToken::new()).await?;
    assert_eq!(
        result.output.termination,
        Termination::Exited { code: Some(0) }
    );
    let output: serde_json::Value = serde_json::from_slice(&result.output.stdout)?;
    assert_eq!(
        output.get("id").and_then(serde_json::Value::as_str),
        Some(id.as_str())
    );
    assert!(
        images
            .0
            .lock()
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    Ok(())
}
