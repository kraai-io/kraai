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
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

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
) -> io::Result<platform::Stream> {
    let started = spawned
        .await
        .map_err(|error| io::Error::other(format!("Nushell host was not spawned: {error}")))?;
    tokio::time::timeout_at(started + HANDSHAKE_TIMEOUT, async {
        let mut transport = listener.accept().await?;
        read_greeting(&mut transport).await?;
        Ok(transport)
    })
    .await
    .map_err(|_elapsed| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Nushell host handshake timed out five seconds after being spawned",
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
                "incompatible Nushell host protocol: expected {HOST_PROTOCOL_VERSION}, received {version}. Rebuild Kraai and kraai-nushell-host together with `just build`"
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
        let client = async {
            tokio::time::sleep(HANDSHAKE_TIMEOUT + Duration::from_millis(100)).await;
            spawned_tx
                .send(tokio::time::Instant::now())
                .map_err(|_instant| io::Error::other("handshake stopped before spawn"))?;
            let mut stream = tokio::net::UnixStream::connect(path.join("host.sock")).await?;
            stream.write_u8(HOST_READY).await?;
            stream.write_u32(HOST_PROTOCOL_VERSION).await?;
            Ok::<_, io::Error>(stream)
        };
        let (server, client) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(accept(listener, spawned_rx), client)
        })
        .await?;
        let _server = server?;
        let _client = client?;
        Ok(())
    }
}
