use std::io;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use kraai_sandbox::LaunchPlan;

const TRANSPORT_DESCRIPTOR: RawFd = 20;

pub(crate) type Stream = tokio::net::UnixStream;

pub(crate) struct Listener {
    listener: tokio::net::UnixListener,
    path: PathBuf,
}

impl Listener {
    pub(crate) fn bind(directory: &Path) -> io::Result<Self> {
        let path = directory.join("host.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: tokio::net::UnixListener::from_std(listener)?,
            path,
        })
    }

    pub(crate) fn configure_launch(&mut self, launch: &mut LaunchPlan) {
        launch.arg("--transport").arg(&self.path);
        launch.private_ipc_connect_paths.push(self.path.clone());
        launch
            .private_ipc_connect_descriptors
            .push(TRANSPORT_DESCRIPTOR);
    }

    pub(crate) async fn accept(self) -> io::Result<Stream> {
        self.listener
            .accept()
            .await
            .map(|(stream, _address)| stream)
    }
}

pub(crate) fn connect(path: &Path) -> io::Result<std::fs::File> {
    let socket = transport_socket()?;
    let transport = claim_transport_descriptor(socket, TRANSPORT_DESCRIPTOR)?;
    let address = rustix::net::SocketAddrUnix::new(path)?;
    rustix::net::connect(&transport, &address)?;
    Ok(std::fs::File::from(transport))
}

fn transport_socket() -> rustix::io::Result<OwnedFd> {
    #[cfg(not(target_vendor = "apple"))]
    {
        rustix::net::socket_with(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
    }
    #[cfg(target_vendor = "apple")]
    {
        let socket = rustix::net::socket(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::STREAM,
            None,
        )?;
        rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
        Ok(socket)
    }
}

fn claim_transport_descriptor(socket: OwnedFd, descriptor: RawFd) -> io::Result<OwnedFd> {
    if socket.as_raw_fd() == descriptor {
        return Ok(socket);
    }

    // Host startup is single-threaded. Claim the seccomp-authorized descriptor
    // before constructing the Nushell engine so inherited descriptors cannot
    // force the transport onto a different number.
    match nix::unistd::close(ReservedDescriptor(descriptor)) {
        Ok(()) | Err(nix::errno::Errno::EBADF) => {}
        Err(error) => return Err(error.into()),
    }

    let transport = rustix::io::fcntl_dupfd_cloexec(&socket, descriptor)?;
    if transport.as_raw_fd() != descriptor {
        return Err(io::Error::other(format!(
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

#[cfg(test)]
mod tests {
    use std::os::fd::{AsRawFd, IntoRawFd};

    use super::{claim_transport_descriptor, transport_socket};

    #[test]
    fn transport_descriptor_replaces_an_inherited_collision()
    -> Result<(), Box<dyn std::error::Error>> {
        let socket = transport_socket()?;
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
