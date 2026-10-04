use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Router,
    body::{Body, Bytes},
    http::header,
    routing::{get, post},
};
use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio_util::task::AbortOnDropHandle;

pub enum Reply {
    Text(String),
    Script(String),
    Streaming,
}

impl From<&str> for Reply {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}

pub struct Harness {
    pub root: tempfile::TempDir,
    pub child: Child,
    pub input: Option<ChildStdin>,
    pub output: BufReader<ChildStdout>,
    pub model_requests: Arc<AtomicUsize>,
    _server: AbortOnDropHandle<()>,
}

impl Harness {
    pub async fn new(replies: Vec<Reply>) -> Result<Self> {
        let root = tempfile::tempdir()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let model_requests = Arc::new(AtomicUsize::new(0));
        let received_requests = model_requests.clone();
        let router = Router::new()
            .route(
                "/v1/models",
                get(async || axum::Json(json!({"data":[{"id":"mock-model"}]}))),
            )
            .route(
                "/v1/chat/completions",
                post(move || {
                    let replies = replies.clone();
                    let index = received_requests.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let reply = replies.lock().await.pop_front();
                        let (delta, finish_reason) = match reply {
                            Some(Reply::Text(text)) => (json!({"content":text}), Some("stop")),
                            Some(Reply::Script(input)) => (
                                json!({"tool_calls":[{"index":0,"id":format!("call-{index}"),"type":"function","function":{"name":"kraai_nushell","arguments":json!({"input":input}).to_string()}}]}),
                                Some("tool_calls"),
                            ),
                            Some(Reply::Streaming) => (json!({"content":"partial"}), None),
                            None => (json!({"content":"Unexpected extra model request"}), Some("stop")),
                        };
                        let chunk =
                            json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish_reason}]});
                        let first = format!("data: {chunk}\n\n");
                        let tail = if finish_reason.is_none() {
                            stream::pending::<std::result::Result<Bytes, std::io::Error>>().boxed()
                        } else {
                            stream::iter([Ok(Bytes::from_static(b"data: [DONE]\n\n"))]).boxed()
                        };
                        let body =
                            Body::from_stream(stream::iter([Ok(Bytes::from(first))]).chain(tail));
                        ([(header::CONTENT_TYPE, "text/event-stream")], body)
                    }
                }),
            );
        let server = AbortOnDropHandle::new(tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        }));
        tokio::fs::write(
            root.path().join("providers.toml"),
            format!(
                r#"
[[provider]]
id = "mock"
type = "openai-chat-completions"
base_url = "http://{address}/v1"
api_key = "test-key"
only_listed_models = true

[[model]]
id = "mock-model"
provider_id = "mock"
supports_images = true
"#
            ),
        )
        .await?;
        let (child, input, output) = Self::spawn(root.path())?;
        Ok(Self {
            root,
            child,
            input: Some(input),
            output,
            model_requests,
            _server: server,
        })
    }

    fn spawn(root: &std::path::Path) -> Result<(Child, ChildStdin, BufReader<ChildStdout>)> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kraai-acp"))
            .args([
                "--provider",
                "mock",
                "--model",
                "mock-model",
                "--storage-root",
            ])
            .arg(root.join("state"))
            .arg("--provider-config")
            .arg(root.join("providers.toml"))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take().ok_or_else(|| eyre!("missing stdin"))?;
        let output = BufReader::new(child.stdout.take().ok_or_else(|| eyre!("missing stdout"))?);
        Ok((child, input, output))
    }

    pub async fn send(&mut self, value: Value) -> Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| eyre!("stdin is closed"))?;
        input.write_all(format!("{value}\n").as_bytes()).await?;
        input.flush().await?;
        Ok(())
    }

    pub async fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = tokio::time::timeout(Duration::from_secs(20), self.output.read_line(&mut line))
            .await??;
        if bytes == 0 {
            return Err(eyre!("ACP subprocess closed stdout"));
        }
        Ok(serde_json::from_str(&line)?)
    }

    pub async fn until(
        &mut self,
        predicate: impl Fn(&Value) -> bool + Send + Sync,
    ) -> Result<Vec<Value>> {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut messages = Vec::new();
            loop {
                let value = self.read().await?;
                let done = predicate(&value);
                messages.push(value);
                if done {
                    return Ok(messages);
                }
            }
        })
        .await?
    }

    pub async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Vec<Value>> {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        self.until(|value| value.get("id") == Some(&json!(id)))
            .await
    }

    pub async fn initialize(&mut self) -> Result<Value> {
        let values = self
            .request(
                0,
                "initialize",
                json!({"protocolVersion":2,"clientCapabilities":{}}),
            )
            .await?;
        values
            .last()
            .cloned()
            .ok_or_else(|| eyre!("missing initialize response"))
    }

    pub async fn session(&mut self) -> Result<String> {
        let values = self
            .request(
                1,
                "session/new",
                json!({"cwd":self.root.path(),"mcpServers":[]}),
            )
            .await?;
        values
            .last()
            .and_then(|value| value.pointer("/result/sessionId"))
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| eyre!("missing session ID: {values:?}"))
    }

    pub async fn stop(&mut self) -> Result<()> {
        drop(self.input.take());
        let status = tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await??;
        if !status.success() {
            return Err(eyre!("ACP subprocess failed: {status}"));
        }
        Ok(())
    }

    pub async fn restart(&mut self) -> Result<()> {
        self.stop().await?;
        let (child, input, output) = Self::spawn(self.root.path())?;
        self.child = child;
        self.input = Some(input);
        self.output = output;
        Ok(())
    }
}

pub fn prompt(session: &str) -> Value {
    json!({"sessionId":session,"prompt":[{"type":"text","text":"Run the task"}]})
}
