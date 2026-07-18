use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;

use crate::request::HostRequest;
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub(crate) const TRANSPORT_DESCRIPTOR: RawFd = 20;

pub(crate) async fn write_request(
    writer: &mut (impl AsyncWrite + Unpin),
    request: &HostRequest,
) -> Result<(), WireError> {
    let bytes = serde_json::to_vec(request).map_err(WireError::Serialize)?;
    let length = u64::try_from(bytes.len()).map_err(|_error| WireError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .await
        .map_err(WireError::Io)?;
    writer.write_all(&bytes).await.map_err(WireError::Io)?;
    writer.flush().await.map_err(WireError::Io)
}

pub(crate) fn connect_transport(path: &Path) -> Result<std::fs::File, WireError> {
    let socket = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|error| WireError::Descriptor(error.to_string()))?;
    let transport = rustix::io::fcntl_dupfd_cloexec(&socket, TRANSPORT_DESCRIPTOR)
        .map_err(|error| WireError::Descriptor(error.to_string()))?;
    if transport.as_raw_fd() != TRANSPORT_DESCRIPTOR {
        return Err(WireError::Descriptor(format!(
            "descriptor {TRANSPORT_DESCRIPTOR} was unavailable"
        )));
    }
    drop(socket);
    let address = rustix::net::SocketAddrUnix::new(path)
        .map_err(|error| WireError::Descriptor(error.to_string()))?;
    rustix::net::connect(&transport, &address)
        .map_err(|error| WireError::Descriptor(error.to_string()))?;
    Ok(std::fs::File::from(transport))
}

pub(crate) fn read_request(
    mut transport: std::fs::File,
) -> Result<(HostRequest, std::fs::File), WireError> {
    let mut length = [0_u8; 8];
    transport.read_exact(&mut length).map_err(WireError::Io)?;
    let length =
        usize::try_from(u64::from_be_bytes(length)).map_err(|_error| WireError::FrameTooLarge)?;
    let mut bytes = vec![0_u8; length];
    transport.read_exact(&mut bytes).map_err(WireError::Io)?;
    let request = serde_json::from_slice(&bytes).map_err(WireError::Deserialize)?;
    Ok((request, transport))
}

#[derive(Debug)]
pub(crate) enum WireError {
    Io(std::io::Error),
    Descriptor(String),
    Serialize(serde_json::Error),
    Deserialize(serde_json::Error),
    FrameTooLarge,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "request channel I/O failed: {error}"),
            Self::Descriptor(message) => {
                write!(f, "private transport setup failed: {message}")
            }
            Self::Serialize(error) => write!(f, "request serialization failed: {error}"),
            Self::Deserialize(error) => write!(f, "request deserialization failed: {error}"),
            Self::FrameTooLarge => write!(f, "request frame exceeds this platform's size range"),
        }
    }
}

impl std::error::Error for WireError {}
