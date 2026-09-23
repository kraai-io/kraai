mod protocol;

use std::sync::{Arc, Mutex};

use kraai_command_core::{StateEffectClient, StateEffectError, WebSearchClient};
use kraai_types::{
    CommandInvocationId, ContextStateDelta, ScriptExecutionId, StateEffectAck, StateEffectRequest,
    WebSearchRequest, WebSearchResponse,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use ulid::Ulid;

use crate::effects::StateEffectHandler;
use protocol::{
    HostProtocolError, read_authenticated_sync, read_frame_async, verify_frame,
    write_authenticated_async, write_authenticated_sync,
};

#[derive(Serialize, Deserialize)]
enum Request {
    StateEffect(StateEffectRequest),
    WebSearch(WebSearchRequest),
}

#[derive(Serialize, Deserialize)]
enum Response {
    StateEffect(StateEffectAck),
    WebSearch(Result<WebSearchResponse, String>),
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
            Response::WebSearch(_) => Err(StateEffectError::new("unexpected host response")),
        }
    }
}

impl WebSearchClient for DescriptorClient {
    fn search(&self, request: WebSearchRequest) -> Result<WebSearchResponse, String> {
        match self.call(|_| Request::WebSearch(request))? {
            Response::WebSearch(result) => result,
            Response::StateEffect(_) => Err(String::from("unexpected host response")),
        }
    }
}

pub(crate) async fn serve(
    mut reader: impl AsyncRead + Unpin + Send,
    mut writer: impl AsyncWrite + Unpin + Send,
    execution_id: ScriptExecutionId,
    secret: [u8; 32],
    effects: Arc<dyn StateEffectHandler>,
    web: Arc<dyn kraai_web::WebSearch>,
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
            Request::StateEffect(request) => {
                if request.sequence != frame.sequence {
                    return Err(HostProtocolError::PayloadSequence {
                        frame: frame.sequence,
                        payload: request.sequence,
                    });
                }
                Response::StateEffect(StateEffectAck {
                    error: effects.apply(&request).await.err(),
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
                    web.search(&request).await
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
