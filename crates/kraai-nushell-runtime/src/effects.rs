mod protocol;

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use kraai_command_core::{StateEffectClient, StateEffectError};
use kraai_types::{
    CommandInvocationId, ContextStateDelta, ScriptExecutionId, StateEffectAck, StateEffectRequest,
};
use tokio::io::{AsyncRead, AsyncWrite};
use ulid::Ulid;

use protocol::{
    EffectProtocolError, read_authenticated_sync, read_frame_async, verify_frame,
    write_authenticated_async, write_authenticated_sync,
};

pub trait StateEffectHandler: Send + Sync {
    fn apply<'a>(
        &'a self,
        request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

#[derive(Debug, Default)]
pub struct RejectStateEffects;

impl StateEffectHandler for RejectStateEffects {
    fn apply<'a>(
        &'a self,
        _request: &'a StateEffectRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async {
            Err(String::from(
                "state effects are not enabled for this execution",
            ))
        })
    }
}

pub(crate) struct DescriptorEffectClient {
    execution_id: ScriptExecutionId,
    secret: [u8; 32],
    transport: Mutex<HostTransport>,
}

struct HostTransport {
    channel: std::fs::File,
    next_sequence: u64,
}

impl DescriptorEffectClient {
    pub(crate) fn from_transport(
        execution_id: ScriptExecutionId,
        secret: [u8; 32],
        channel: std::fs::File,
    ) -> Self {
        Self {
            execution_id,
            secret,
            transport: Mutex::new(HostTransport {
                channel,
                next_sequence: 1,
            }),
        }
    }
}

impl StateEffectClient for DescriptorEffectClient {
    fn apply(
        &self,
        command_id: &'static str,
        deltas: Vec<ContextStateDelta>,
    ) -> Result<(), StateEffectError> {
        let mut transport = self.transport.lock().map_err(|error| {
            StateEffectError::new(format!("state effect channel poisoned: {error}"))
        })?;
        let sequence = transport.next_sequence;
        transport.next_sequence = transport
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| StateEffectError::new("state effect sequence exhausted"))?;
        let invocation_id = CommandInvocationId::new(Ulid::generate());
        let request = StateEffectRequest {
            sequence,
            invocation_id: invocation_id.clone(),
            command_id: String::from(command_id),
            deltas,
        };
        write_authenticated_sync(
            &mut transport.channel,
            &self.execution_id,
            sequence,
            &request,
            &self.secret,
        )
        .map_err(|error| StateEffectError::new(error.to_string()))?;
        let (ack_sequence, ack): (u64, StateEffectAck) =
            read_authenticated_sync(&mut transport.channel, &self.execution_id, &self.secret)
                .map_err(|error| StateEffectError::new(error.to_string()))?;
        drop(transport);
        if ack_sequence != sequence {
            return Err(StateEffectError::new(format!(
                "state effect acknowledgment sequence {ack_sequence} did not match {sequence}"
            )));
        }
        if ack.invocation_id != invocation_id {
            return Err(StateEffectError::new(
                "state effect acknowledgment invocation id did not match",
            ));
        }
        if let Some(error) = ack.error {
            return Err(StateEffectError::new(error));
        }
        Ok(())
    }
}

pub(crate) async fn serve_effects(
    mut events: impl AsyncRead + Unpin + Send,
    mut acknowledgments: impl AsyncWrite + Unpin + Send,
    execution_id: ScriptExecutionId,
    secret: [u8; 32],
    handler: std::sync::Arc<dyn StateEffectHandler>,
) -> Result<(), EffectProtocolError> {
    let mut expected_sequence = 1_u64;
    loop {
        let Some(frame) = read_frame_async::<StateEffectRequest>(&mut events).await? else {
            return Ok(());
        };
        verify_frame(&frame, &execution_id, &secret)?;
        if frame.sequence != expected_sequence {
            return Err(EffectProtocolError::Sequence {
                expected: expected_sequence,
                received: frame.sequence,
            });
        }
        if frame.payload.sequence != frame.sequence {
            return Err(EffectProtocolError::PayloadSequence {
                frame: frame.sequence,
                payload: frame.payload.sequence,
            });
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(EffectProtocolError::SequenceExhausted)?;
        let error = handler.apply(&frame.payload).await.err();
        let ack = StateEffectAck {
            invocation_id: frame.payload.invocation_id,
            error,
        };
        write_authenticated_async(
            &mut acknowledgments,
            &execution_id,
            frame.sequence,
            &ack,
            &secret,
        )
        .await?;
    }
}
