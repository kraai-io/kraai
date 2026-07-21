use std::io::Read;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};
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
    let transport = claim_transport_descriptor(socket, TRANSPORT_DESCRIPTOR)?;
    let address = rustix::net::SocketAddrUnix::new(path)
        .map_err(|error| WireError::Descriptor(error.to_string()))?;
    rustix::net::connect(&transport, &address)
        .map_err(|error| WireError::Descriptor(error.to_string()))?;
    Ok(std::fs::File::from(transport))
}

fn claim_transport_descriptor(socket: OwnedFd, descriptor: RawFd) -> Result<OwnedFd, WireError> {
    if socket.as_raw_fd() == descriptor {
        return Ok(socket);
    }

    // Host startup is single-threaded. Claim the seccomp-authorized descriptor
    // before constructing the Nushell engine so inherited descriptors cannot
    // force the transport onto a different number.
    match nix::unistd::close(ReservedDescriptor(descriptor)) {
        Ok(()) | Err(nix::errno::Errno::EBADF) => {}
        Err(error) => return Err(WireError::Descriptor(error.to_string())),
    }

    let transport = rustix::io::fcntl_dupfd_cloexec(&socket, descriptor)
        .map_err(|error| WireError::Descriptor(error.to_string()))?;
    if transport.as_raw_fd() != descriptor {
        return Err(WireError::Descriptor(format!(
            "descriptor {descriptor} was claimed concurrently"
        )));
    }
    Ok(transport)
}

struct ReservedDescriptor(RawFd);

impl IntoRawFd for ReservedDescriptor {
    fn into_raw_fd(self) -> RawFd {
        self.0
    }
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

#[cfg(test)]
mod tests {
    use std::os::fd::{AsRawFd, IntoRawFd};

    use super::claim_transport_descriptor;

    #[test]
    fn transport_descriptor_replaces_an_inherited_collision()
    -> Result<(), Box<dyn std::error::Error>> {
        let socket = rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )?;
        let occupied = std::fs::File::open("/dev/null")?;
        let target = occupied.into_raw_fd();
        if socket.as_raw_fd() == target {
            return Err(std::io::Error::other("fixture descriptors collided").into());
        }

        let transport = claim_transport_descriptor(socket, target)?;

        if transport.as_raw_fd() != target {
            return Err(std::io::Error::other("transport used the wrong descriptor").into());
        }
        if !rustix::io::fcntl_getfd(&transport)?.contains(rustix::io::FdFlags::CLOEXEC) {
            return Err(
                std::io::Error::other("transport descriptor was inherited across exec").into(),
            );
        }
        Ok(())
    }
}
