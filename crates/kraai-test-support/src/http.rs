//! HTTP fixture servers that only serve the requests their test sends.
//!
//! Other local clients can reach a fixture's port. T3 Code's preview port scanner, for one,
//! sends `GET /` to every port a process started from a T3 thread listens on. A fixture that
//! treats each connection as its test's request lets such a probe use up a scripted response
//! or fail a request assertion, so fixtures serve their test's traffic under a path and
//! answer everything else with 404.

use std::io::{self, Read};
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// Path a fixture serves when the code under test lets the test choose the URL.
pub const PATH: &str = "/kraai-fixture";

const MAX_REQUEST_LINE: usize = 8192;
const NOT_FOUND: &[u8] =
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// A loopback listener that only yields connections requesting a path under its prefix.
pub struct Listener {
    inner: TcpListener,
    prefix: String,
    pending: JoinSet<Option<(TcpStream, SocketAddr)>>,
}

impl Listener {
    /// Binds a loopback port serving request targets that start with `prefix`.
    pub async fn bind(prefix: &str) -> io::Result<Self> {
        Ok(Self {
            inner: TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?,
            prefix: prefix.to_owned(),
            pending: JoinSet::new(),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// The fixture's URL, ending in its prefix.
    pub fn url(&self) -> io::Result<String> {
        Ok(format!("http://{}{}", self.local_addr()?, self.prefix))
    }

    /// Accepts the next connection whose request targets the prefix, answering every other
    /// request with 404. The returned stream has not been read from.
    pub async fn accept(&mut self) -> io::Result<(TcpStream, SocketAddr)> {
        loop {
            tokio::select! {
                accepted = self.inner.accept() => {
                    let (stream, address) = accepted?;
                    let prefix = self.prefix.clone();
                    self.pending.spawn(async move {
                        expected(stream, &prefix).await.map(|stream| (stream, address))
                    });
                }
                Some(Ok(Some(accepted))) = self.pending.join_next() => return Ok(accepted),
            }
        }
    }
}

async fn expected(mut stream: TcpStream, prefix: &str) -> Option<TcpStream> {
    let mut line = vec![0; MAX_REQUEST_LINE];
    let mut peeked = 0;
    loop {
        let read = stream.peek(&mut line).await.ok()?;
        if let Some(end) = line_end(line.get(..read)?) {
            if targets(line.get(..end)?, prefix) {
                return Some(stream);
            }
            break;
        }
        if read == 0 || read == line.len() {
            break;
        }
        if read == peeked {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        peeked = read;
    }
    let _ = stream.write_all(NOT_FOUND).await;
    let _ = stream.shutdown().await;
    None
}

/// Reports whether a connection queued on the nonblocking `listener` requested a target
/// under `prefix`, for tests that check a sandboxed process could not reach the network.
pub fn received(listener: &std::net::TcpListener, prefix: &str) -> io::Result<bool> {
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        let mut line = Vec::new();
        let mut chunk = [0; 1024];
        while line.len() < MAX_REQUEST_LINE && line_end(&line).is_none() {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => line.extend_from_slice(chunk.get(..read).unwrap_or_default()),
            }
        }
        if line_end(&line)
            .and_then(|end| line.get(..end))
            .is_some_and(|line| targets(line, prefix))
        {
            return Ok(true);
        }
    }
}

fn line_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|pair| pair == b"\r\n")
}

fn targets(request_line: &[u8], prefix: &str) -> bool {
    request_line
        .split(|byte| *byte == b' ')
        .nth(1)
        .is_some_and(|target| target.starts_with(prefix.as_bytes()))
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests assert fixture behavior and propagate socket errors"
)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn connect(address: SocketAddr, request: String) -> io::Result<TcpStream> {
        let mut client = TcpStream::connect(address).await?;
        for chunk in request.as_bytes().chunks(3) {
            client.write_all(chunk).await?;
            tokio::task::yield_now().await;
        }
        Ok(client)
    }

    #[tokio::test]
    async fn stray_requests_get_404_and_the_expected_one_is_left_unread() -> io::Result<()> {
        let mut listener = Listener::bind(PATH).await?;
        let address = listener.local_addr()?;
        let request = format!("POST {PATH}/chat?x=1 HTTP/1.1\r\nContent-Length: 2\r\n\r\n{{}}");
        let sent = request.clone();
        let clients = tokio::spawn(async move {
            let mut stray =
                connect(address, "GET / HTTP/1.1\r\nUser-Agent: node\r\n\r\n".into()).await?;
            let mut response = Vec::new();
            stray.read_to_end(&mut response).await?;
            let expected = connect(address, sent).await?;
            Ok::<_, io::Error>((response, expected))
        });
        let (mut stream, _) = crate::hang_guard(listener.accept()).await??;
        let mut received = vec![0; request.len()];
        stream.read_exact(&mut received).await?;
        assert_eq!(received, request.as_bytes());
        let (response, _expected) = clients.await??;
        assert!(response.starts_with(b"HTTP/1.1 404"));
        Ok(())
    }

    #[tokio::test]
    async fn silent_connections_do_not_block_the_expected_one() -> io::Result<()> {
        let mut listener = Listener::bind(PATH).await?;
        let address = listener.local_addr()?;
        let _silent = TcpStream::connect(address).await?;
        let _client = connect(address, format!("GET {PATH} HTTP/1.1\r\n\r\n")).await?;
        crate::hang_guard(listener.accept()).await??;
        Ok(())
    }

    #[test]
    fn received_ignores_requests_outside_the_prefix() -> io::Result<()> {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let mut stray = std::net::TcpStream::connect(address)?;
        stray.write_all(b"GET / HTTP/1.1\r\n\r\n")?;
        assert!(!received(&listener, PATH)?);
        let mut leaked = std::net::TcpStream::connect(address)?;
        leaked.write_all(format!("GET {PATH} HTTP/1.1\r\n\r\n").as_bytes())?;
        assert!(received(&listener, PATH)?);
        Ok(())
    }
}
