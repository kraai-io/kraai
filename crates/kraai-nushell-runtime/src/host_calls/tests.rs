#![expect(
    clippy::panic_in_result_fn,
    reason = "transport tests propagate setup errors and assert boundaries"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use super::*;

#[derive(Default)]
struct RecordingImages(Mutex<Vec<(u64, Vec<u8>)>>);

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
        sequence: u64,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, String>> + Send + '_>> {
        Box::pin(async move {
            let byte_length = bytes.len() as u64;
            self.0
                .lock()
                .map_err(|error| error.to_string())?
                .push((sequence, bytes));
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
async fn image_requests_require_enabled_command_and_preserve_authenticated_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    for enabled in [false, true] {
        let images = Arc::new(RecordingImages::default());
        let recorded = images.clone();
        let execution_id = ScriptExecutionId::new("image-test");
        let server_execution_id = execution_id.clone();
        let secret = rand::random::<[u8; 32]>();
        let (client, server) = tokio::io::duplex(8192);
        let (reader, writer) = tokio::io::split(server);
        let task = tokio::spawn(async move {
            let commands = if enabled {
                vec![String::from(kraai_commands::view_image::METADATA.id)]
            } else {
                vec![]
            };
            serve(
                reader,
                writer,
                server_execution_id,
                secret,
                HostServices {
                    mcp: Arc::new(kraai_mcp::McpManager::default()),
                    effects: Arc::new(crate::RejectStateEffects),
                    web: Arc::new(kraai_web::ExaSearch::default()),
                    images,
                    output: tokio::sync::mpsc::unbounded_channel().0,
                },
                &commands,
                &CancellationToken::new(),
            )
            .await
        });
        let (mut reader, mut writer) = tokio::io::split(client);
        let bytes = vec![0, 255, 128, 13, 10];
        write_authenticated_async(
            &mut writer,
            &execution_id,
            1,
            &Request::ImageAttachment {
                base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
            &secret,
        )
        .await?;
        let response = read_authenticated_async::<Response>(&mut reader, &execution_id, &secret)
            .await?
            .ok_or("missing response")?;
        assert_eq!(response.sequence, 1);
        let Response::ImageAttachment(result) = response.payload else {
            return Err("wrong response type".into());
        };
        if enabled {
            let attachment = result?;
            assert_eq!(attachment.byte_length, bytes.len() as u64);
            assert_eq!(
                *recorded.0.lock().map_err(|error| error.to_string())?,
                vec![(1, bytes)]
            );
        } else {
            assert!(result.is_err());
            assert!(
                recorded
                    .0
                    .lock()
                    .map_err(|error| error.to_string())?
                    .is_empty()
            );
        }
        for (sequence, id, should_succeed) in [
            (2, "a".repeat(64), enabled),
            (3, "../outside".to_string(), false),
        ] {
            write_authenticated_async(
                &mut writer,
                &execution_id,
                sequence,
                &Request::ExistingImage { id },
                &secret,
            )
            .await?;
            let response =
                read_authenticated_async::<Response>(&mut reader, &execution_id, &secret)
                    .await?
                    .ok_or("missing response")?;
            assert_eq!(response.sequence, sequence);
            let Response::ImageAttachment(result) = response.payload else {
                return Err("wrong response type".into());
            };
            assert_eq!(result.is_ok(), should_succeed);
        }
        drop(writer);
        drop(reader);
        task.await??;
    }
    Ok(())
}

#[tokio::test]
async fn stopped_channels_forward_queued_output_but_serve_no_requests()
-> Result<(), Box<dyn std::error::Error>> {
    let images = Arc::new(RecordingImages::default());
    let recorded = images.clone();
    let execution_id = ScriptExecutionId::new("stopped-test");
    let secret = rand::random::<[u8; 32]>();
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (_client_reader, mut client_writer) = tokio::io::split(client);
    let frames = [
        OutputFrame::ListEnd { id: 2, list: 1 },
        OutputFrame::ListEnd { id: 4, list: 3 },
    ];
    for (sequence, frame) in (1..).zip(frames.clone()) {
        write_authenticated_async(
            &mut client_writer,
            &execution_id,
            sequence,
            &Request::Output(frame),
            &secret,
        )
        .await?;
    }
    write_authenticated_async(
        &mut client_writer,
        &execution_id,
        3,
        &Request::ImageAttachment {
            base64: base64::engine::general_purpose::STANDARD.encode([0]),
        },
        &secret,
    )
    .await?;
    drop(client_writer);
    let stopped = CancellationToken::new();
    stopped.cancel();
    let (output, mut forwarded) = tokio::sync::mpsc::unbounded_channel();
    let (reader, writer) = tokio::io::split(server);
    serve(
        reader,
        writer,
        execution_id,
        secret,
        HostServices {
            mcp: Arc::new(kraai_mcp::McpManager::default()),
            effects: Arc::new(crate::RejectStateEffects),
            web: Arc::new(kraai_web::ExaSearch::default()),
            images,
            output,
        },
        &[String::from(kraai_commands::view_image::METADATA.id)],
        &stopped,
    )
    .await?;
    let mut received = Vec::new();
    while let Ok(frame) = forwarded.try_recv() {
        received.push(frame);
    }
    assert_eq!(received, frames);
    assert!(
        recorded
            .0
            .lock()
            .map_err(|error| error.to_string())?
            .is_empty()
    );
    Ok(())
}

#[test]
fn image_decoding_rejects_invalid_and_oversized_payloads() {
    assert!(decode_image("not an image encoding!").is_err());
    assert!(decode_image(&"A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1)).is_err());
}

#[test]
fn maximum_image_payload_survives_authenticated_framing() -> Result<(), Box<dyn std::error::Error>>
{
    let bytes = vec![255; MAX_IMAGE_BYTES];
    let request = Request::ImageAttachment {
        base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    };
    let execution_id = ScriptExecutionId::new("image-max");
    let secret = rand::random::<[u8; 32]>();
    let mut wire = Vec::new();
    write_authenticated_sync(&mut wire, &execution_id, 1, &request, &secret)?;
    let (sequence, request) =
        read_authenticated_sync::<Request>(&mut wire.as_slice(), &execution_id, &secret)?;
    assert_eq!(sequence, 1);
    let Request::ImageAttachment { base64 } = request else {
        return Err("wrong request type".into());
    };
    assert_eq!(decode_image(&base64)?, bytes);
    Ok(())
}
