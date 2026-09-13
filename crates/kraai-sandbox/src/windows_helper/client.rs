use std::io;
use std::os::windows::io::{AsRawHandle, IntoRawHandle};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::NamedPipeClient;

#[expect(
    unsafe_code,
    reason = "transfer an overlapped pipe handle with no server-creation access to Tokio"
)]
pub(super) fn connect() -> io::Result<NamedPipeClient> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_APPEND_DATA, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        SECURITY_IDENTIFICATION,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode((FILE_GENERIC_READ | FILE_GENERIC_WRITE) & !FILE_APPEND_DATA)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(super::PIPE)?;
    unsafe { NamedPipeClient::from_raw_handle(file.into_raw_handle()) }
}

#[derive(Debug)]
pub(crate) struct Lease(NamedPipeClient);

impl Lease {
    pub(crate) async fn acquire(nonce: &[u8; 16]) -> io::Result<Self> {
        tokio::time::timeout(super::TIMEOUT, async {
            let mut pipe = loop {
                match connect() {
                    Ok(pipe) => break pipe,
                    Err(error) if error.raw_os_error() == Some(231) => {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await
                    }
                    Err(error) => return Err(error),
                }
            };
            super::setup::verify_server(pipe.as_raw_handle())?;
            pipe.write_all(super::MAGIC).await?;
            pipe.write_all(nonce).await?;
            super::check(pipe.read_u32_le().await?)?;
            Ok(Self(pipe))
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(crate) async fn release(mut self) -> io::Result<()> {
        tokio::time::timeout(super::TIMEOUT, async {
            self.0.write_u8(0).await?;
            super::check(self.0.read_u32_le().await?)
        })
        .await
        .map_err(io::Error::other)?
    }
}
