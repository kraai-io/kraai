use std::future::Future;
use std::io::{Read, Write};
use std::pin::Pin;
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use kraai_command_core::{StateEffectClient, StateEffectError};
use kraai_types::{
    CommandInvocationId, ContextStateDelta, ScriptExecutionId, StateEffectAck, StateEffectRequest,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ulid::Ulid;

type HmacSha256 = Hmac<Sha256>;

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

#[derive(Debug, Serialize, Deserialize)]
struct AuthenticatedFrame<T> {
    execution_id: ScriptExecutionId,
    sequence: u64,
    payload: T,
    mac: [u8; 32],
}

#[derive(Serialize)]
struct UnsignedFrame<'a, T> {
    execution_id: &'a ScriptExecutionId,
    sequence: u64,
    payload: &'a T,
}

fn signed_frame<T: Serialize + Clone>(
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<AuthenticatedFrame<T>, EffectProtocolError> {
    let bytes = serde_json::to_vec(&UnsignedFrame {
        execution_id,
        sequence,
        payload,
    })
    .map_err(EffectProtocolError::Serialize)?;
    let mut hmac = HmacSha256::new_from_slice(secret)
        .map_err(|error| EffectProtocolError::Authentication(error.to_string()))?;
    hmac.update(&bytes);
    let mac: [u8; 32] = hmac.finalize().into_bytes().into();
    Ok(AuthenticatedFrame {
        execution_id: execution_id.clone(),
        sequence,
        payload: payload.clone(),
        mac,
    })
}

fn verify_frame<T: Serialize>(
    frame: &AuthenticatedFrame<T>,
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<(), EffectProtocolError> {
    if &frame.execution_id != execution_id {
        return Err(EffectProtocolError::ExecutionId);
    }
    let bytes = serde_json::to_vec(&UnsignedFrame {
        execution_id: &frame.execution_id,
        sequence: frame.sequence,
        payload: &frame.payload,
    })
    .map_err(EffectProtocolError::Serialize)?;
    let mut hmac = HmacSha256::new_from_slice(secret)
        .map_err(|error| EffectProtocolError::Authentication(error.to_string()))?;
    hmac.update(&bytes);
    hmac.verify_slice(&frame.mac)
        .map_err(|_error| EffectProtocolError::Authentication(String::from("invalid frame MAC")))
}

fn write_authenticated_sync<T: Serialize + Clone>(
    writer: &mut impl Write,
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<(), EffectProtocolError> {
    let frame = signed_frame(execution_id, sequence, payload, secret)?;
    let bytes = serde_json::to_vec(&frame).map_err(EffectProtocolError::Serialize)?;
    write_length_sync(writer, bytes.len())?;
    writer.write_all(&bytes).map_err(EffectProtocolError::Io)?;
    writer.flush().map_err(EffectProtocolError::Io)
}

fn read_authenticated_sync<T: DeserializeOwned + Serialize>(
    reader: &mut impl Read,
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<(u64, T), EffectProtocolError> {
    let length = read_length_sync(reader)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(EffectProtocolError::Io)?;
    let frame: AuthenticatedFrame<T> =
        serde_json::from_slice(&bytes).map_err(EffectProtocolError::Deserialize)?;
    verify_frame(&frame, execution_id, secret)?;
    Ok((frame.sequence, frame.payload))
}

async fn write_authenticated_async<T: Serialize + Clone + Send + Sync>(
    writer: &mut (impl AsyncWrite + Unpin + Send),
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<(), EffectProtocolError> {
    let frame = signed_frame(execution_id, sequence, payload, secret)?;
    let bytes = serde_json::to_vec(&frame).map_err(EffectProtocolError::Serialize)?;
    let length = u32::try_from(bytes.len()).map_err(|_error| EffectProtocolError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .await
        .map_err(EffectProtocolError::Io)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(EffectProtocolError::Io)?;
    writer.flush().await.map_err(EffectProtocolError::Io)
}

async fn read_frame_async<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<Option<AuthenticatedFrame<T>>, EffectProtocolError> {
    let mut length = [0_u8; 4];
    match reader.read(&mut length[..1]).await {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) if is_clean_channel_close(&error) => return Ok(None),
        Err(error) => return Err(EffectProtocolError::Io(error)),
    }
    reader
        .read_exact(&mut length[1..])
        .await
        .map_err(EffectProtocolError::Io)?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_error| EffectProtocolError::FrameTooLarge)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(EffectProtocolError::Io)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(EffectProtocolError::Deserialize)
}

fn is_clean_channel_close(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
    )
}

fn write_length_sync(writer: &mut impl Write, length: usize) -> Result<(), EffectProtocolError> {
    let length = u32::try_from(length).map_err(|_error| EffectProtocolError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(EffectProtocolError::Io)
}

fn read_length_sync(reader: &mut impl Read) -> Result<usize, EffectProtocolError> {
    let mut length = [0_u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(EffectProtocolError::Io)?;
    usize::try_from(u32::from_be_bytes(length)).map_err(|_error| EffectProtocolError::FrameTooLarge)
}

#[derive(Debug)]
pub(crate) enum EffectProtocolError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
    Deserialize(serde_json::Error),
    Authentication(String),
    ExecutionId,
    Sequence { expected: u64, received: u64 },
    PayloadSequence { frame: u64, payload: u64 },
    SequenceExhausted,
    FrameTooLarge,
}

impl std::fmt::Display for EffectProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "state effect channel I/O failed: {error}"),
            Self::Serialize(error) => write!(f, "state effect serialization failed: {error}"),
            Self::Deserialize(error) => write!(f, "state effect deserialization failed: {error}"),
            Self::Authentication(message) => {
                write!(f, "state effect authentication failed: {message}")
            }
            Self::ExecutionId => write!(f, "state effect execution id did not match"),
            Self::Sequence { expected, received } => write!(
                f,
                "state effect sequence {received} did not match expected {expected}"
            ),
            Self::PayloadSequence { frame, payload } => write!(
                f,
                "state effect payload sequence {payload} did not match frame sequence {frame}"
            ),
            Self::SequenceExhausted => write!(f, "state effect sequence exhausted"),
            Self::FrameTooLarge => {
                write!(f, "state effect frame exceeds the protocol length range")
            }
        }
    }
}

impl std::error::Error for EffectProtocolError {}

#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "protocol tests use direct failure messages for fixture construction"
)]
mod tests {
    use super::{EffectProtocolError, signed_frame, verify_frame};
    use kraai_types::{CommandInvocationId, ScriptExecutionId, StateEffectAck};

    #[test]
    fn authenticated_frames_reject_payload_execution_and_mac_tampering() {
        let execution_id = ScriptExecutionId::new("execution");
        let secret = [7_u8; 32];
        let payload = StateEffectAck {
            invocation_id: CommandInvocationId::new("invocation"),
            error: None,
        };
        let mut frame = signed_frame(&execution_id, 1, &payload, &secret)
            .unwrap_or_else(|error| panic!("unable to sign test frame: {error}"));
        assert!(verify_frame(&frame, &execution_id, &secret).is_ok());

        frame.payload.error = Some(String::from("forged"));
        assert!(matches!(
            verify_frame(&frame, &execution_id, &secret),
            Err(EffectProtocolError::Authentication(_))
        ));

        let frame = signed_frame(&execution_id, 1, &payload, &secret)
            .unwrap_or_else(|error| panic!("unable to sign test frame: {error}"));
        assert!(matches!(
            verify_frame(&frame, &ScriptExecutionId::new("other"), &secret),
            Err(EffectProtocolError::ExecutionId)
        ));
        assert!(matches!(
            verify_frame(&frame, &execution_id, &[8_u8; 32]),
            Err(EffectProtocolError::Authentication(_))
        ));
    }
}
