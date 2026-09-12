use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use kraai_sandbox::LaunchPlan;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
use windows_sys::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};
use windows_sys::Win32::System::Pipes::GetNamedPipeInfo;

pub(crate) type Stream = NamedPipeServer;

pub(crate) struct Listener {
    server: NamedPipeServer,
    client: Option<OwnedHandle>,
}

impl Listener {
    pub(crate) fn bind(_directory: &Path) -> io::Result<Self> {
        let name = format!(r"\\.\pipe\kraai-host-{:032x}", rand::random::<u128>());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .create(&name)?;
        let client = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(name)?;
        Ok(Self {
            server,
            client: Some(client.into()),
        })
    }

    pub(crate) fn configure_launch(&mut self, launch: &mut LaunchPlan) {
        if let Some(client) = self.client.take() {
            launch
                .arg("--transport")
                .arg((client.as_raw_handle() as usize).to_string());
            launch.private_ipc_handles.push(client);
        }
    }

    pub(crate) async fn accept(self) -> io::Result<Stream> {
        self.server.connect().await?;
        Ok(self.server)
    }
}

#[expect(
    unsafe_code,
    reason = "the host must claim a validated inherited Windows pipe handle and disable inheritance"
)]
pub(crate) fn connect(endpoint: &Path) -> io::Result<File> {
    let value = endpoint
        .to_str()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value != 0 && *value != usize::MAX)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid transport handle"))?;
    let handle = value as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: These calls validate the inherited handle without dereferencing it.
    if unsafe {
        GetNamedPipeInfo(
            handle,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
        || unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The launcher transferred this valid pipe handle to the initial host.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::windows::io::{AsRawHandle, IntoRawHandle};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation,
    };

    use super::{Listener, connect};

    #[tokio::test]
    #[expect(
        unsafe_code,
        reason = "the test inspects the inheritance flags on a live owned pipe handle"
    )]
    async fn claiming_transport_prevents_inheritance_by_external_commands() -> io::Result<()> {
        let mut listener = Listener::bind(Path::new("."))?;
        let client = listener
            .client
            .take()
            .ok_or_else(|| io::Error::other("missing client handle"))?;
        // SAFETY: The test owns this live handle for the duration of the call.
        if unsafe {
            SetHandleInformation(
                client.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let endpoint = (client.into_raw_handle() as usize).to_string();
        let transport = connect(Path::new(&endpoint))?;
        let mut flags = 0;
        // SAFETY: The file owns a live pipe handle and flags is a valid output pointer.
        if unsafe { GetHandleInformation(transport.as_raw_handle(), &mut flags) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & HANDLE_FLAG_INHERIT != 0 {
            return Err(io::Error::other("transport handle remains inheritable"));
        }
        listener.server.connect().await?;
        Ok(())
    }
}
