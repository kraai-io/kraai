use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::request::HOST_PROTOCOL_VERSION;

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
    transport.write_all(&HOST_PROTOCOL_VERSION.to_be_bytes())?;
    transport.flush()?;
    Ok(transport)
}

pub(crate) async fn accept(
    listener: Listener,
    spawned: tokio::sync::oneshot::Receiver<tokio::time::Instant>,
    startup_timeout: Duration,
) -> io::Result<platform::Stream> {
    let started = spawned
        .await
        .map_err(|error| io::Error::other(format!("Nushell host was not spawned: {error}")))?;
    let deadline = started.checked_add(startup_timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Nushell host startup timeout exceeds the clock range",
        )
    })?;
    tokio::time::timeout_at(deadline, async {
        let mut transport = listener.accept().await?;
        read_greeting(&mut transport).await?;
        Ok(transport)
    })
    .await
    .map_err(|_elapsed| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("Nushell host handshake timed out {startup_timeout:?} after being spawned"),
        )
    })?
}

async fn read_greeting(transport: &mut (impl tokio::io::AsyncRead + Unpin)) -> io::Result<()> {
    if transport.read_u8().await? != HOST_READY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Nushell host sent an invalid transport greeting",
        ));
    }
    let version = transport.read_u32().await?;
    if version != HOST_PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "incompatible Nushell host protocol: expected {HOST_PROTOCOL_VERSION}, received {version}. Rebuild the runtime and its configured Nushell host together"
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
