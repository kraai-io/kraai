use std::io;
use std::ops::Deref;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use agent_client_protocol::{Client, ConnectTo, ConnectionTo, Lines, Role, schema::v1 as acp};
use futures::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, io::BufReader};
use tokio_util::sync::CancellationToken;

const OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const ENVELOPE_BYTES: usize = 128;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct Budget {
    bytes: Arc<AtomicUsize>,
    failed: CancellationToken,
    limit: usize,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Arc::default(),
            failed: CancellationToken::new(),
            limit,
        }
    }

    fn reserve(&self, bytes: usize) -> agent_client_protocol::Result<()> {
        if self.failed.is_cancelled()
            || self
                .bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current
                        .checked_add(bytes)
                        .filter(|total| *total <= self.limit)
                })
                .is_err()
        {
            self.failed.cancel();
            return Err(crate::error::internal("ACP output capacity exceeded"));
        }
        Ok(())
    }

    fn release(&self, bytes: usize) {
        self.bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
}

pub(crate) struct Connection {
    inner: ConnectionTo<Client>,
    budget: Budget,
}

impl Connection {
    pub(crate) fn new(inner: ConnectionTo<Client>, budget: Budget) -> Self {
        Self { inner, budget }
    }

    pub(crate) fn send_notification(
        &self,
        notification: acp::SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        let bytes = serialized_size(&notification)
            .map_err(|error| crate::error::internal(error.to_string()))?
            + ENVELOPE_BYTES;
        self.budget.reserve(bytes)?;
        if let Err(error) = self.inner.send_notification(notification) {
            self.budget.release(bytes);
            return Err(error);
        }
        Ok(())
    }
}

impl Deref for Connection {
    type Target = ConnectionTo<Client>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub struct Stdio {
    pub(crate) budget: Budget,
}

impl Stdio {
    #[must_use]
    pub fn new() -> Self {
        Self {
            budget: Budget::new(OUTPUT_BYTES),
        }
    }
}

impl Default for Stdio {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct SerializedSize(usize);

impl io::Write for SerializedSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_size(value: &impl serde::Serialize) -> serde_json::Result<usize> {
    let mut size = SerializedSize::default();
    serde_json::to_writer(&mut size, value)?;
    Ok(size.0)
}

#[derive(serde::Deserialize)]
struct WireFrame<'a> {
    method: Option<&'a str>,
    #[serde(borrow)]
    params: Option<&'a serde_json::value::RawValue>,
}

async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin + Send),
    line: &str,
    budget: &Budget,
    timeout: Duration,
) -> io::Result<()> {
    if line.len() > OUTPUT_BYTES {
        budget.failed.cancel();
        return Err(io::Error::other("ACP output frame exceeds capacity"));
    }
    let value: WireFrame<'_> = serde_json::from_str(line)?;
    let reservation = if value.method == Some("session/update") {
        value
            .params
            .ok_or_else(|| io::Error::other("ACP update omitted params"))?
            .get()
            .len()
            + ENVELOPE_BYTES
    } else {
        0
    };
    tokio::time::timeout(timeout, async {
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    })
    .await
    .map_err(|error| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("ACP client stopped reading output: {error}"),
        )
    })??;
    budget.release(reservation);
    Ok(())
}

impl<R: Role> ConnectTo<R> for Stdio {
    async fn connect_to(
        self,
        client: impl ConnectTo<R::Counterpart>,
    ) -> agent_client_protocol::Result<()> {
        let stdin = blocking::Unblock::new(std::io::stdin());
        let stdout = blocking::Unblock::with_capacity(64 * 1024, std::io::stdout());
        let sink = futures::sink::unfold(
            (stdout, self.budget.clone()),
            async move |(mut writer, budget), line: String| {
                write_frame(&mut writer, &line, &budget, WRITE_TIMEOUT).await?;
                Ok::<_, io::Error>((writer, budget))
            },
        );
        let protocol =
            ConnectTo::<R>::connect_to(Lines::new(sink, BufReader::new(stdin).lines()), client);
        tokio::select! {
            biased;
            () = self.budget.failed.cancelled() => Err(crate::error::internal("ACP output capacity exceeded")),
            result = protocol => result,
        }
    }
}

#[cfg(test)]
mod tests;
