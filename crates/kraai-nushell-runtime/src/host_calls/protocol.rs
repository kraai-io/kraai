use std::io::{Read, Write};

use hmac::{Hmac, KeyInit, Mac};
use kraai_execution_types::ScriptExecutionId;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_FRAME_BYTES: usize =
    kraai_execution_types::image::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 64 * 1024;
const LENGTH_BYTES: usize = 4;
const MAC_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
pub(super) struct Frame<T> {
    execution_id: ScriptExecutionId,
    pub(super) sequence: u64,
    pub(super) payload: T,
}

#[derive(Serialize)]
struct FrameRef<'a, T> {
    execution_id: &'a ScriptExecutionId,
    sequence: u64,
    payload: &'a T,
}

fn keyed(secret: &[u8; 32]) -> Result<HmacSha256, HostProtocolError> {
    HmacSha256::new_from_slice(secret)
        .map_err(|error| HostProtocolError::Authentication(error.to_string()))
}

fn seal<T: Serialize>(
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<Vec<u8>, HostProtocolError> {
    let mut bytes = vec![0_u8; LENGTH_BYTES + MAC_BYTES];
    rmp_serde::encode::write_named(
        &mut bytes,
        &FrameRef {
            execution_id,
            sequence,
            payload,
        },
    )
    .map_err(HostProtocolError::Serialize)?;
    let length = bytes.len() - LENGTH_BYTES;
    validate_frame_length(length)?;
    let length = u32::try_from(length).map_err(|_error| HostProtocolError::FrameTooLarge)?;
    let (header, body) = bytes.split_at_mut(LENGTH_BYTES + MAC_BYTES);
    let (length_bytes, mac) = header.split_at_mut(LENGTH_BYTES);
    length_bytes.copy_from_slice(&length.to_be_bytes());
    mac.copy_from_slice(&keyed(secret)?.chain_update(body).finalize().into_bytes());
    Ok(bytes)
}

fn open<T: DeserializeOwned>(
    bytes: &[u8],
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<Frame<T>, HostProtocolError> {
    let (mac, body) = bytes
        .split_at_checked(MAC_BYTES)
        .ok_or_else(|| HostProtocolError::Authentication(String::from("frame is too short")))?;
    keyed(secret)?
        .chain_update(body)
        .verify_slice(mac)
        .map_err(|_error| HostProtocolError::Authentication(String::from("invalid frame MAC")))?;
    let frame: Frame<T> = rmp_serde::from_slice(body).map_err(HostProtocolError::Deserialize)?;
    if &frame.execution_id != execution_id {
        return Err(HostProtocolError::ExecutionId);
    }
    Ok(frame)
}

pub(super) fn write_authenticated_sync<T: Serialize>(
    writer: &mut impl Write,
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<(), HostProtocolError> {
    let bytes = seal(execution_id, sequence, payload, secret)?;
    writer.write_all(&bytes).map_err(HostProtocolError::Io)?;
    writer.flush().map_err(HostProtocolError::Io)
}

pub(super) fn read_authenticated_sync<T: DeserializeOwned>(
    reader: &mut impl Read,
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<(u64, T), HostProtocolError> {
    let length = read_length_sync(reader)?;
    validate_frame_length(length)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .map_err(HostProtocolError::Io)?;
    let frame = open::<T>(&bytes, execution_id, secret)?;
    Ok((frame.sequence, frame.payload))
}

pub(super) async fn write_authenticated_async<T: Serialize + Send + Sync>(
    writer: &mut (impl AsyncWrite + Unpin + Send),
    execution_id: &ScriptExecutionId,
    sequence: u64,
    payload: &T,
    secret: &[u8; 32],
) -> Result<(), HostProtocolError> {
    let bytes = seal(execution_id, sequence, payload, secret)?;
    writer
        .write_all(&bytes)
        .await
        .map_err(HostProtocolError::Io)?;
    writer.flush().await.map_err(HostProtocolError::Io)
}

pub(super) async fn read_authenticated_async<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
    execution_id: &ScriptExecutionId,
    secret: &[u8; 32],
) -> Result<Option<Frame<T>>, HostProtocolError> {
    let mut length = [0_u8; LENGTH_BYTES];
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
    validate_frame_length(length)?;
    let mut bytes = vec![0_u8; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(HostProtocolError::Io)?;
    open(&bytes, execution_id, secret).map(Some)
}

fn is_clean_channel_close(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
    )
}

fn validate_frame_length(length: usize) -> Result<(), HostProtocolError> {
    if length > MAX_FRAME_BYTES {
        Err(HostProtocolError::FrameTooLarge)
    } else {
        Ok(())
    }
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
    Serialize(rmp_serde::encode::Error),
    Deserialize(rmp_serde::decode::Error),
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
                write!(f, "host frame exceeds the protocol size limit")
            }
        }
    }
}

impl std::error::Error for HostProtocolError {}

#[cfg(test)]
mod tests;
