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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn preparation_time_does_not_consume_the_handshake_deadline()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let directory = kraai_sandbox::PrivateTempConfig::default().reserve()?;
        let path = directory.path().ok_or("missing private temp")?;
        let listener = Listener::bind(path)?;
        let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
        let startup_timeout = Duration::from_millis(250);
        let client = async {
            tokio::time::sleep(startup_timeout + Duration::from_millis(100)).await;
            spawned_tx
                .send(tokio::time::Instant::now())
                .map_err(|_instant| io::Error::other("handshake stopped before spawn"))?;
            let mut stream = tokio::net::UnixStream::connect(path.join("host.sock")).await?;
            stream.write_u8(HOST_READY).await?;
            stream.write_u32(HOST_PROTOCOL_VERSION).await?;
            Ok::<_, io::Error>(stream)
        };
        let (server, client) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(accept(listener, spawned_rx, startup_timeout), client)
        })
        .await?;
        let _server = server?;
        let _client = client?;
        Ok(())
    }

    #[tokio::test]
    async fn custom_startup_timeout_limits_the_handshake()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let directory = kraai_sandbox::PrivateTempConfig::default().reserve()?;
        let path = directory.path().ok_or("missing private temp")?;
        let listener = Listener::bind(path)?;
        let (spawned_tx, spawned_rx) = tokio::sync::oneshot::channel();
        spawned_tx
            .send(tokio::time::Instant::now())
            .map_err(|_instant| io::Error::other("handshake stopped before spawn"))?;
        let startup_timeout = Duration::from_millis(25);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            accept(listener, spawned_rx, startup_timeout),
        )
        .await?;
        let error = result.err().ok_or("missing startup timeout")?;
        if error.kind() != io::ErrorKind::TimedOut
            || error.to_string() != "Nushell host handshake timed out 25ms after being spawned"
        {
            return Err(io::Error::other(format!("unexpected startup timeout: {error}")).into());
        }
        Ok(())
    }
}
