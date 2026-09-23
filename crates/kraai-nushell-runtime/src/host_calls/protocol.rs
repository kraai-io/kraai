use std::io::{Read, Write};

use hmac::{Hmac, KeyInit, Mac};
use kraai_types::ScriptExecutionId;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct AuthenticatedFrame<T> {
    execution_id: ScriptExecutionId,
    pub(super) sequence: u64,
    pub(super) payload: T,
    mac: [u8; 32],
}

#[derive(Serialize)]
struct UnsignedFrame<'a, T> {
    execution_id: &'a ScriptExecutionId,
    sequence: u64,
    payload: &'a T,
}

fn frame_mac<T: Serialize>(
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<HmacSha256, HostProtocolError> {
    let bytes = serde_json::to_vec(&UnsignedFrame {
        execution_id,
        sequence,
        payload,
    })
    .map_err(HostProtocolError::Serialize)?;
    let mut hmac = HmacSha256::new_from_slice(secret)
        .map_err(|error| HostProtocolError::Authentication(error.to_string()))?;
    hmac.update(&bytes);
    Ok(hmac)
}

fn signed_frame<'a, T: Serialize>(
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &'a T,
    secret: &[u8; 32],
) -> Result<AuthenticatedFrame<&'a T>, HostProtocolError> {
    let hmac = frame_mac(execution_id, sequence, payload, secret)?;
    let mac: [u8; 32] = hmac.finalize().into_bytes().into();
    Ok(AuthenticatedFrame {
        execution_id: execution_id.clone(),
        sequence,
        payload,
        mac,
    })
}

pub(super) fn verify_frame<T: Serialize>(
    frame: &AuthenticatedFrame<T>,
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<(), HostProtocolError> {
    if &frame.execution_id != execution_id {
        return Err(HostProtocolError::ExecutionId);
    }
    let hmac = frame_mac(&frame.execution_id, frame.sequence, &frame.payload, secret)?;
    hmac.verify_slice(&frame.mac)
        .map_err(|_error| HostProtocolError::Authentication(String::from("invalid frame MAC")))
}

pub(super) fn write_authenticated_sync<T: Serialize>(
    writer: &mut impl Write,
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<(), HostProtocolError> {
    let frame = signed_frame(execution_id, sequence, payload, secret)?;
    let bytes = serde_json::to_vec(&frame).map_err(HostProtocolError::Serialize)?;
    write_length_sync(writer, bytes.len())?;
    writer.write_all(&bytes).map_err(HostProtocolError::Io)?;
    writer.flush().map_err(HostProtocolError::Io)
}

pub(super) fn read_authenticated_sync<T: DeserializeOwned + Serialize>(
    reader: &mut impl Read,
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<(u64, T), HostProtocolError> {
    let length = read_length_sync(reader)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(HostProtocolError::Io)?;
    let frame: AuthenticatedFrame<T> =
        serde_json::from_slice(&bytes).map_err(HostProtocolError::Deserialize)?;
    verify_frame(&frame, execution_id, secret)?;
    Ok((frame.sequence, frame.payload))
}

pub(super) async fn write_authenticated_async<T: Serialize + Send + Sync>(
    writer: &mut (impl AsyncWrite + Unpin + Send),
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<(), HostProtocolError> {
    let frame = signed_frame(execution_id, sequence, payload, secret)?;
    let bytes = serde_json::to_vec(&frame).map_err(HostProtocolError::Serialize)?;
    let length = u32::try_from(bytes.len()).map_err(|_error| HostProtocolError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .await
        .map_err(HostProtocolError::Io)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(HostProtocolError::Io)?;
    writer.flush().await.map_err(HostProtocolError::Io)
}

pub(super) async fn read_frame_async<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<Option<AuthenticatedFrame<T>>, HostProtocolError> {
    let mut length = [0_u8; 4];
    match reader.read(&mut length[..1]).await {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) if is_clean_channel_close(&error) => return Ok(None),
        Err(error) => return Err(HostProtocolError::Io(error)),
    }
    reader
        .read_exact(&mut length[1..])
        .await
        .map_err(HostProtocolError::Io)?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_error| HostProtocolError::FrameTooLarge)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(HostProtocolError::Io)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(HostProtocolError::Deserialize)
}

fn is_clean_channel_close(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
    )
}

fn write_length_sync(writer: &mut impl Write, length: usize) -> Result<(), HostProtocolError> {
    let length = u32::try_from(length).map_err(|_error| HostProtocolError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(HostProtocolError::Io)
}

fn read_length_sync(reader: &mut impl Read) -> Result<usize, HostProtocolError> {
    let mut length = [0_u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(HostProtocolError::Io)?;
    usize::try_from(u32::from_be_bytes(length)).map_err(|_error| HostProtocolError::FrameTooLarge)
}

#[derive(Debug)]
pub(crate) enum HostProtocolError {
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

impl std::fmt::Display for HostProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "host channel I/O failed: {error}"),
            Self::Serialize(error) => write!(f, "host serialization failed: {error}"),
            Self::Deserialize(error) => write!(f, "host deserialization failed: {error}"),
            Self::Authentication(message) => {
                write!(f, "host authentication failed: {message}")
            }
            Self::ExecutionId => write!(f, "host execution id did not match"),
            Self::Sequence { expected, received } => write!(
                f,
                "host sequence {received} did not match expected {expected}"
            ),
            Self::PayloadSequence { frame, payload } => write!(
                f,
                "host payload sequence {payload} did not match frame sequence {frame}"
            ),
            Self::SequenceExhausted => write!(f, "host sequence exhausted"),
            Self::FrameTooLarge => {
                write!(f, "host frame exceeds the protocol length range")
            }
        }
    }
}

impl std::error::Error for HostProtocolError {}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "protocol tests propagate fixture errors and assert frame integrity"
)]
mod tests {
    use super::{
        AuthenticatedFrame, HostProtocolError, UnsignedFrame, signed_frame, verify_frame,
        write_authenticated_async, write_authenticated_sync,
    };
    use hmac::{Hmac, KeyInit, Mac};
    use kraai_types::{CommandInvocationId, ScriptExecutionId, StateEffectAck};

    #[tokio::test]
    async fn serialization_failure_preserves_errors_and_writes_no_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        struct InvalidPayload;

        impl serde::Serialize for InvalidPayload {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid effect payload"))
            }
        }

        let execution_id = ScriptExecutionId::new("execution");
        let secret = rand::random::<[u8; 32]>();
        let payload = InvalidPayload;
        let expected = match serde_json::to_vec(&UnsignedFrame {
            execution_id: &execution_id,
            sequence: 1,
            payload: &payload,
        }) {
            Err(error) => error.to_string(),
            Ok(_) => return Err("invalid payload unexpectedly serialized".into()),
        };

        let mut synchronous = Vec::new();
        let mut asynchronous = Vec::new();
        let results = [
            write_authenticated_sync(&mut synchronous, &execution_id, 1, &payload, &secret),
            write_authenticated_async(&mut asynchronous, &execution_id, 1, &payload, &secret).await,
        ];
        for result in results {
            let Err(HostProtocolError::Serialize(error)) = result else {
                return Err("writer did not preserve serialization error".into());
            };
            assert_eq!(error.to_string(), expected);
        }
        assert!(synchronous.is_empty());
        assert!(asynchronous.is_empty());

        let frame = AuthenticatedFrame {
            execution_id: execution_id.clone(),
            sequence: 1,
            payload,
            mac: [0; 32],
        };
        assert!(matches!(
            verify_frame(&frame, &ScriptExecutionId::new("other"), &secret),
            Err(HostProtocolError::ExecutionId),
        ));
        let Err(HostProtocolError::Serialize(error)) = verify_frame(&frame, &execution_id, &secret)
        else {
            return Err("verification did not preserve serialization error".into());
        };
        assert_eq!(error.to_string(), expected);
        Ok(())
    }

    #[test]
    fn authenticated_frames_reject_payload_execution_and_mac_tampering()
    -> Result<(), Box<dyn std::error::Error>> {
        let execution_id = ScriptExecutionId::new("execution");
        let secret = rand::random::<[u8; 32]>();
        let payload = StateEffectAck {
            invocation_id: CommandInvocationId::new("invocation"),
            error: None,
        };
        let bytes = serde_json::to_vec(&signed_frame(&execution_id, 1, &payload, &secret)?)?;
        let mut frame: AuthenticatedFrame<StateEffectAck> = serde_json::from_slice(&bytes)?;
        assert!(verify_frame(&frame, &execution_id, &secret).is_ok());

        frame.payload.error = Some(String::from("forged"));
        assert!(matches!(
            verify_frame(&frame, &execution_id, &secret),
            Err(HostProtocolError::Authentication(_))
        ));

        let frame = signed_frame(&execution_id, 1, &payload, &secret)?;
        assert!(matches!(
            verify_frame(&frame, &ScriptExecutionId::new("other"), &secret),
            Err(HostProtocolError::ExecutionId)
        ));
        assert!(matches!(
            verify_frame(&frame, &execution_id, &secret.map(|byte| byte ^ 1)),
            Err(HostProtocolError::Authentication(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_writers_preserve_frame_bytes() -> Result<(), Box<dyn std::error::Error>>
    {
        let execution_id = ScriptExecutionId::new("execution");
        let secret = rand::random::<[u8; 32]>();
        let payload = StateEffectAck {
            invocation_id: CommandInvocationId::new("invocation"),
            error: None,
        };
        let mut hmac = Hmac::<sha2::Sha256>::new_from_slice(&secret)?;
        hmac.update(br#"{"execution_id":"execution","sequence":1,"payload":{"invocation_id":"invocation","error":null}}"#);
        let mac: [u8; 32] = hmac.finalize().into_bytes().into();
        let frame = format!(
            r#"{{"execution_id":"execution","sequence":1,"payload":{{"invocation_id":"invocation","error":null}},"mac":{}}}"#,
            serde_json::to_string(&mac)?,
        );
        let mut expected = u32::try_from(frame.len())?.to_be_bytes().to_vec();
        expected.extend_from_slice(frame.as_bytes());

        let mut synchronous = Vec::new();
        write_authenticated_sync(&mut synchronous, &execution_id, 1, &payload, &secret)?;
        assert_eq!(synchronous, expected);

        let mut asynchronous = Vec::new();
        write_authenticated_async(&mut asynchronous, &execution_id, 1, &payload, &secret).await?;
        assert_eq!(asynchronous, expected);
        Ok(())
    }
}
