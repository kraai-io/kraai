mod protocol;

use std::sync::{Arc, Mutex};

use base64::Engine;
use kraai_command_core::{
    ImageAttachmentClient, StateEffectClient, StateEffectError, WebSearchClient,
};
use kraai_types::{
    CommandInvocationId, ContextStateDelta, ImageAttachment, ScriptExecutionId, StateEffectAck,
    StateEffectRequest, WebSearchRequest, WebSearchResponse,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use ulid::Ulid;

use crate::effects::StateEffectHandler;
use crate::images::ImageAttachmentHandler;
use kraai_types::image::{MAX_IMAGE_BYTES, validate_image_id};
use protocol::{
    HostProtocolError, read_authenticated_sync, read_frame_async, verify_frame,
    write_authenticated_async, write_authenticated_sync,
};

#[derive(Serialize, Deserialize)]
enum Request {
    StateEffect(StateEffectRequest),
    WebSearch(WebSearchRequest),
    ImageAttachment { base64: String },
    ExistingImage { id: String },
}

#[derive(Serialize, Deserialize)]
enum Response {
    StateEffect(StateEffectAck),
    WebSearch(Result<WebSearchResponse, String>),
    ImageAttachment(Result<ImageAttachment, String>),
}

pub(crate) struct DescriptorClient {
    execution_id: ScriptExecutionId,
    secret: [u8; 32],
    transport: Mutex<Transport>,
}

struct Transport {
    channel: std::fs::File,
    next_sequence: u64,
}

impl DescriptorClient {
    pub(crate) fn from_transport(
        execution_id: ScriptExecutionId,
        secret: [u8; 32],
        channel: std::fs::File,
    ) -> Self {
        Self {
            execution_id,
            secret,
            transport: Mutex::new(Transport {
                channel,
                next_sequence: 1,
            }),
        }
    }

    fn call(&self, request: impl FnOnce(u64) -> Request) -> Result<Response, String> {
        let mut transport = self
            .transport
            .lock()
            .map_err(|error| format!("host channel poisoned: {error}"))?;
        let sequence = transport.next_sequence;
        transport.next_sequence = sequence.checked_add(1).ok_or("host sequence exhausted")?;
        write_authenticated_sync(
            &mut transport.channel,
            &self.execution_id,
            sequence,
            &request(sequence),
            &self.secret,
        )
        .map_err(|error| error.to_string())?;
        let (received, response) =
            read_authenticated_sync(&mut transport.channel, &self.execution_id, &self.secret)
                .map_err(|error| error.to_string())?;
        drop(transport);
        if received != sequence {
            return Err(String::from("host response sequence mismatch"));
        }
        Ok(response)
    }
}

impl StateEffectClient for DescriptorClient {
    fn apply(
        &self,
        command_id: &'static str,
        deltas: Vec<ContextStateDelta>,
    ) -> Result<(), StateEffectError> {
        let invocation_id = CommandInvocationId::new(Ulid::generate());
        let response = self
            .call(|sequence| {
                Request::StateEffect(StateEffectRequest {
                    sequence,
                    invocation_id: invocation_id.clone(),
                    command_id: String::from(command_id),
                    deltas,
                })
            })
            .map_err(StateEffectError::new)?;
        match response {
            Response::StateEffect(ack) => {
                if ack.invocation_id != invocation_id {
                    return Err(StateEffectError::new("host response invocation mismatch"));
                }
                match ack.error {
                    Some(error) => Err(StateEffectError::new(error)),
                    None => Ok(()),
                }
            }
            Response::WebSearch(_) | Response::ImageAttachment(_) => {
                Err(StateEffectError::new("unexpected host response"))
            }
        }
    }
}

impl WebSearchClient for DescriptorClient {
    fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, String> {
        match self.call(|_| Request::WebSearch(request))? {
            Response::WebSearch(result) => result,
            Response::StateEffect(_) | Response::ImageAttachment(_) => {
                Err(String::from("unexpected host response"))
            }
        }
    }
}

impl ImageAttachmentClient for DescriptorClient {
    fn attach_existing(&self, id: String) -> Result<ImageAttachment, String> {
        validate_image_id(&id)?;
        match self.call(|_| Request::ExistingImage { id })? {
            Response::ImageAttachment(result) => result,
            _ => Err(String::from("unexpected host response")),
        }
    }

    fn attach(&self, bytes: Vec<u8>) -> Result<ImageAttachment, String> {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(format!("image exceeds the {MAX_IMAGE_BYTES} byte limit"));
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        match self.call(|_| Request::ImageAttachment { base64: encoded })? {
            Response::ImageAttachment(result) => result,
            _ => Err(String::from("unexpected host response")),
        }
    }
}

pub(crate) struct HostServices {
    pub(crate) effects: Arc<dyn StateEffectHandler>,
    pub(crate) web: Arc<dyn kraai_web::WebSearch>,
    pub(crate) images: Arc<dyn ImageAttachmentHandler>,
}

pub(crate) async fn serve(
    mut reader: impl AsyncRead + Unpin + Send,
    mut writer: impl AsyncWrite + Unpin + Send,
    execution_id: ScriptExecutionId,
    secret: [u8; 32],
    services: HostServices,
    active_commands: &[String],
) -> Result<(), HostProtocolError> {
    let mut expected_sequence = 1_u64;
    while let Some(frame) = read_frame_async::<Request>(&mut reader).await? {
        verify_frame(&frame, &execution_id, &secret)?;
        if frame.sequence != expected_sequence {
            return Err(HostProtocolError::Sequence {
                expected: expected_sequence,
                received: frame.sequence,
            });
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(HostProtocolError::SequenceExhausted)?;
        let response = match frame.payload {
            Request::ExistingImage { id } => {
                let result = if !active_commands
                    .iter()
                    .any(|command| command == kraai_command_catalog::VIEW_IMAGE.id)
                {
                    Err(String::from("view image command is not enabled"))
                } else if let Err(error) = validate_image_id(&id) {
                    Err(error)
                } else {
                    services.images.attach_existing(frame.sequence, id).await
                };
                Response::ImageAttachment(result)
            }
            Request::ImageAttachment { base64 } => {
                let result = if !active_commands
                    .iter()
                    .any(|id| id == kraai_command_catalog::VIEW_IMAGE.id)
                {
                    Err(String::from("view image command is not enabled"))
                } else {
                    match decode_image(&base64) {
                        Ok(bytes) => services.images.attach(frame.sequence, bytes).await,
                        Err(error) => Err(error),
                    }
                };
                Response::ImageAttachment(result)
            }
            Request::StateEffect(request) => {
                if request.sequence != frame.sequence {
                    return Err(HostProtocolError::PayloadSequence {
                        frame: frame.sequence,
                        payload: request.sequence,
                    });
                }
                Response::StateEffect(StateEffectAck {
                    error: services.effects.apply(&request).await.err(),
                    invocation_id: request.invocation_id,
                })
            }
            Request::WebSearch(request) => {
                let result = if !active_commands
                    .iter()
                    .any(|id| id == kraai_command_catalog::WEB_SEARCH.id)
                {
                    Err(String::from("web search command is not enabled"))
                } else if let Err(error) = request.validate() {
                    Err(error)
                } else {
                    services.web.search(&request).await
                };
                Response::WebSearch(result)
            }
        };
        write_authenticated_async(
            &mut writer,
            &execution_id,
            frame.sequence,
            &response,
            &secret,
        )
        .await?;
    }
    Ok(())
}

fn decode_image(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(format!("image exceeds the {MAX_IMAGE_BYTES} byte limit"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid image encoding: {error}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("image exceeds the {MAX_IMAGE_BYTES} byte limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
