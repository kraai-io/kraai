use std::io::{self, Write};
use std::path::Path;

use tokio::io::AsyncReadExt;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub(crate) use platform::Listener;

const HOST_READY: u8 = 1;

pub(crate) fn connect(endpoint: &Path) -> io::Result<std::fs::File> {
    let mut transport = platform::connect(endpoint)?;
    transport.write_all(&[HOST_READY])?;
    transport.flush()?;
    Ok(transport)
}

pub(crate) async fn accept(listener: Listener) -> io::Result<platform::Stream> {
    let mut transport = listener.accept().await?;
    if transport.read_u8().await? != HOST_READY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Nushell host sent an invalid transport greeting",
        ));
    }
    Ok(transport)
}
