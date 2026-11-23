//! T3 Code's preview port scanner sends `GET /` to every port a T3 thread's process listens on,
//! so fixtures serve their test's traffic under a path and answer everything else with 404.

use std::io::{self, Read};
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

pub const PATH: &str = "/kraai-fixture";

const MAX_REQUEST_LINE: usize = 8192;
const NOT_FOUND: &[u8] =
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

pub struct Listener {
    inner: TcpListener,
    prefix: String,
    pending: JoinSet<Option<(TcpStream, SocketAddr)>>,
}

impl Listener {
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

    pub fn url(&self) -> io::Result<String> {
        Ok(format!("http://{}{}", self.local_addr()?, self.prefix))
    }

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
mod tests;
