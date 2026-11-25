use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use agent_client_protocol::{Agent, Client, Lines, v1::ConnectionTo};
use axum::{
    Router,
    body::{Body, Bytes},
    routing::{get, post},
};
use futures::{StreamExt, stream};
use kraai_runtime::{RuntimeBuilder, RuntimeStartupState, SubmitMessageOutcome};
use kraai_types::{AssistantItem, SessionId};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::task::AbortOnDropHandle;

use super::*;
use crate::transport::Connection;

enum Reply {
    Text(&'static str),
    Held(&'static str, &'static str, Arc<Notify>),
    Script(&'static str),
}

struct Fixture {
    runtime: RuntimeHandle,
    id: SessionId,
    session: Arc<crate::session::Session>,
    inner: ConnectionTo<Client>,
    lines: Arc<std::sync::Mutex<Vec<String>>>,
    requests: Arc<AtomicUsize>,
    providers: std::path::PathBuf,
    _protocol: AbortOnDropHandle<agent_client_protocol::Result<()>>,
    _server: AbortOnDropHandle<std::io::Result<()>>,
    root: tempfile::TempDir,
}

fn router(replies: Vec<Reply>, requests: Arc<AtomicUsize>) -> Router {
    let replies = Arc::new(std::sync::Mutex::new(VecDeque::from(replies)));
    Router::new()
        .route(
            "/models",
            get(async || axum::Json(json!({"data":[{"id":"mock-model"}]}))),
        )
        .route(
            "/chat/completions",
            post(move || {
                let index = requests.fetch_add(1, Ordering::SeqCst);
                let reply = replies.lock().unwrap().pop_front();
                async move {
                    let body = match reply {
                        Some(Reply::Text(text)) => {
                            stream::iter([Ok::<_, std::io::Error>(sse(&text_chunk(text, true), true))]).boxed()
                        }
                        Some(Reply::Held(start, tail, release)) => {
                            stream::iter([Ok(sse(&text_chunk(start, false), false))])
                                .chain(stream::once(async move {
                                    release.notified().await;
                                    Ok(sse(&text_chunk(tail, true), true))
                                }))
                                .boxed()
                        }
                        Some(Reply::Script(input)) => {
                            let call = json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call-{index}"),"type":"function","function":{"name":"kraai_nushell","arguments":json!({"input":input}).to_string()}}]},"finish_reason":"tool_calls"}]});
                            stream::iter([Ok(sse(&call, true))]).boxed()
                        }
                        None => stream::iter([Ok(sse(&text_chunk("unexpected", true), true))])
                            .boxed(),
                    };
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(body),
                    )
                }
            }),
        )
}

fn sse(chunk: &Value, done: bool) -> Bytes {
    let done = if done { "data: [DONE]\n\n" } else { "" };
    Bytes::from(format!("data: {chunk}\n\n{done}"))
}

fn text_chunk(text: &str, last: bool) -> Value {
    json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":if last {Some("stop")} else {None}}]})
}

fn options() -> crate::Options {
    crate::Options {
        options: Vec::new(),
        provider: Some("mock".into()),
        model: Some("mock-model".into()),
        profile: None,
    }
}

async fn build_runtime(root: &std::path::Path, providers: &std::path::Path) -> RuntimeHandle {
    let runtime = RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .storage_root(root.join("state"))
        .provider_config_path(providers.to_path_buf())
        .mcp_config_path(root.join("mcp.toml"))
        .build_on(&tokio::runtime::Handle::current());
    let startup = runtime.wait_for_startup().await.unwrap();
    assert!(
        startup == RuntimeStartupState::ready(),
        "Runtime fixture did not become ready: {startup:?}"
    );
    runtime
}

impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let router = router(replies, requests.clone());
        let server =
            AbortOnDropHandle::new(tokio::spawn(
                async move { axum::serve(listener, router).await },
            ));
        let providers = root.path().join("providers.toml");
        tokio::fs::write(
            &providers,
            format!(
                r#"
[[provider]]
id = "mock"
type = "openai-chat-completions"
base_url = "http://{address}"
api_key = "test"
only_listed_models = true

[[model]]
id = "mock-model"
provider_id = "mock"
"#
            ),
        )
        .await
        .unwrap();
        let runtime = build_runtime(root.path(), &providers).await;
        let (id, session) = crate::session::create(
            &runtime,
            &options(),
            acp::NewSessionRequest::new(root.path().to_path_buf()),
            Arc::default(),
        )
        .await
        .unwrap();
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = futures::sink::unfold(lines.clone(), async |lines, line: String| {
            if let Ok(mut captured) = lines.lock() {
                captured.push(line);
            }
            Ok::<_, std::io::Error>(lines)
        });
        let transport = Lines::new(sink, stream::pending::<std::io::Result<String>>());
        let (connection_tx, connection_rx) = tokio::sync::oneshot::channel();
        let protocol = AbortOnDropHandle::new(tokio::spawn(Agent.v1().connect_with(
            transport,
            async move |connection| {
                connection_tx
                    .send(connection)
                    .map_err(|_connection| error::internal("connection fixture closed"))?;
                futures::future::pending::<agent_client_protocol::Result<()>>().await
            },
        )));
        let inner = connection_rx.await.unwrap();
        Self {
            runtime,
            id,
            session,
            inner,
            lines,
            requests,
            providers,
            _protocol: protocol,
            _server: server,
            root,
        }
    }

    fn connection(&self) -> Connection {
        Connection::new(self.inner.clone(), crate::Stdio::new().budget)
    }

    fn follow_with(&self, events: broadcast::Receiver<RuntimeEvent>) {
        let (release, released) = oneshot::channel();
        self.session.install_follower(
            Follower::new(
                self.runtime.clone(),
                events,
                Renderer::new(self.id.clone(), self.connection(), 0),
                None,
            )
            .spawn(released)
            .unwrap(),
        );
        release.send(()).unwrap();
    }

    fn follow(&self) {
        self.follow_with(self.runtime.subscribe_session(&self.id));
    }

    async fn other_client(&self) -> RuntimeHandle {
        build_runtime(self.root.path(), &self.providers).await
    }

    fn messages(&self) -> Vec<Value> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn updates(&self, kind: &str, text: &str) -> usize {
        self.messages()
            .iter()
            .filter(|value| {
                value.pointer("/params/update/sessionUpdate") == Some(&json!(kind))
                    && value
                        .pointer("/params/update/content/text")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value.contains(text))
            })
            .count()
    }

    async fn until(&self, what: &str, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        kraai_test_support::hang_guard(async {
            loop {
                let messages = self.messages();
                if done(&messages) {
                    return messages;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("timed out waiting for {what}: {:#?}", self.messages()))
    }

    async fn until_update(&self, kind: &str, text: &str) {
        self.until(&format!("{kind} {text}"), |_| self.updates(kind, text) > 0)
            .await;
    }

    async fn send(&self, runtime: &RuntimeHandle, text: &str) -> SubmitMessageOutcome {
        runtime
            .send_message(
                self.id.clone(),
                text.into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await
            .unwrap()
    }

    async fn prompt(&self, text: &str) -> Result<acp::PromptResponse> {
        crate::prompt::run(
            &self.runtime,
            self.session.begin_turn().unwrap(),
            acp::PromptRequest::new(self.id.to_string(), vec![content::text(text)]),
            self.connection(),
        )
        .await
    }

    async fn idle(&self, runtime: &RuntimeHandle) {
        kraai_test_support::hang_guard(async {
            while runtime
                .get_session_snapshot(self.id.clone())
                .await
                .unwrap()
                .session
                .is_running
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn another_clients_turn_streams_to_an_idle_editor() {
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(vec![Reply::Held(
        "other-start",
        "other-tail",
        release.clone(),
    )])
    .await;
    fixture.follow();
    fixture.send(&fixture.runtime, "from another client").await;
    fixture
        .until_update("agent_message_chunk", "other-start")
        .await;
    release.notify_one();
    fixture
        .until_update("agent_message_chunk", "other-tail")
        .await;
    assert_eq!(
        fixture.updates("user_message_chunk", "from another client"),
        1
    );
    assert_eq!(fixture.updates("agent_message_chunk", "other-start"), 1);
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn prompt_is_refused_while_another_client_runs_a_turn() {
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(vec![
        Reply::Held("other-start", "other-tail", release.clone()),
        Reply::Text("own-reply"),
    ])
    .await;
    fixture.follow();
    fixture.send(&fixture.runtime, "other").await;
    fixture
        .until_update("agent_message_chunk", "other-start")
        .await;
    let refused = kraai_test_support::hang_guard(fixture.prompt("own"))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        refused.message.contains("running a turn"),
        "{}",
        refused.message
    );
    let snapshot = fixture
        .runtime
        .get_session_snapshot(fixture.id.clone())
        .await
        .unwrap();
    assert!(snapshot.session.is_running);
    assert_eq!(snapshot.queued_messages, 0);
    release.notify_one();
    fixture.idle(&fixture.runtime).await;
    let response = fixture.prompt("own").await.unwrap();
    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    assert_eq!(fixture.updates("agent_message_chunk", "own-reply"), 1);
    assert_eq!(fixture.updates("user_message_chunk", "own"), 0);
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_mid_turn_follows_the_live_turn() {
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(vec![
        Reply::Held("other-start", "other-tail", release.clone()),
        Reply::Text("own-reply"),
    ])
    .await;
    let other = fixture.other_client().await;
    assert!(other.load_session(fixture.id.clone()).await.unwrap());
    fixture.send(&other, "from another client").await;
    kraai_test_support::hang_guard(async {
        loop {
            let snapshot = fixture
                .runtime
                .get_session_snapshot(fixture.id.clone())
                .await
                .unwrap();
            if snapshot.session.is_running
                && snapshot
                    .history
                    .values()
                    .any(|message| message.display_text().contains("other-start"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let server = crate::Server {
        budget: crate::Stdio::new().budget,
        runtime: fixture.runtime.clone(),
        options: options(),
        initialized: AtomicBool::new(true),
        sessions: tokio::sync::Mutex::new(BTreeMap::new()),
        config_updated: Arc::default(),
    };
    let connection = fixture.connection();
    let session = {
        let prepared = server
            .load(
                acp::LoadSessionRequest::new(fixture.id.to_string(), fixture.root.path()),
                &connection,
            )
            .await
            .unwrap();
        let session = prepared.session.clone();
        prepared.respond(|_response| Ok(())).unwrap();
        session
    };
    fixture
        .until_update("user_message_chunk", "from another client")
        .await;
    fixture
        .until_update("agent_message_chunk", "other-start")
        .await;
    release.notify_one();
    fixture
        .until_update("agent_message_chunk", "other-tail")
        .await;
    assert_eq!(
        fixture.updates("user_message_chunk", "from another client"),
        1
    );
    assert_eq!(fixture.updates("agent_message_chunk", "other-start"), 1);
    fixture.idle(&fixture.runtime).await;
    let response = crate::prompt::run(
        &fixture.runtime,
        session.begin_turn().unwrap(),
        acp::PromptRequest::new(fixture.id.to_string(), vec![content::text("own")]),
        connection,
    )
    .await
    .unwrap();
    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    assert_eq!(fixture.updates("agent_message_chunk", "own-reply"), 1);
    other.shutdown().await.unwrap();
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn approval_answered_elsewhere_resolves_the_editors_request() {
    let fixture = Fixture::new(vec![
        Reply::Script("# timeout=10min permissions=no-sandbox\nprint 'must-not-run'"),
        Reply::Text("after denial"),
    ])
    .await;
    fixture.follow();
    fixture.send(&fixture.runtime, "other").await;
    let messages = fixture
        .until("permission request", |messages| {
            messages
                .iter()
                .any(|value| value.get("method") == Some(&json!("session/request_permission")))
        })
        .await;
    let request = messages
        .iter()
        .find(|value| value.get("method") == Some(&json!("session/request_permission")))
        .and_then(|value| value.get("id"))
        .cloned()
        .unwrap();
    let pending = fixture
        .runtime
        .get_pending_script(fixture.id.clone())
        .await
        .unwrap()
        .unwrap();
    fixture
        .runtime
        .deny_script(fixture.id.clone(), pending.execution_id)
        .await
        .unwrap();
    fixture
        .until("cancelled permission request", |messages| {
            messages.iter().any(|value| {
                value.get("method") == Some(&json!("$/cancel_request"))
                    && value.pointer("/params/requestId") == Some(&request)
            })
        })
        .await;
    fixture
        .until_update("agent_message_chunk", "after denial")
        .await;
    assert!(fixture.messages().iter().any(|value| {
        value.pointer("/params/update/toolCallId") == Some(&json!(pending.call_id))
            && value.pointer("/params/update/status") == Some(&json!("failed"))
    }));
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn dismissing_another_clients_approval_denies_only_that_script() {
    let fixture = Fixture::new(vec![
        Reply::Script("# timeout=10min permissions=no-sandbox\nprint 'must-not-run'"),
        Reply::Text("after dismissal"),
    ])
    .await;
    let mut follower = Follower::new(
        fixture.runtime.clone(),
        fixture.runtime.subscribe_session(&fixture.id),
        Renderer::new(fixture.id.clone(), fixture.connection(), 0),
        None,
    );
    fixture.send(&fixture.runtime, "other").await;
    let pending = kraai_test_support::hang_guard(async {
        loop {
            if let Some(pending) = fixture
                .runtime
                .get_pending_script(fixture.id.clone())
                .await
                .unwrap()
            {
                return pending;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    follower.request(pending).unwrap();
    follower
        .answer(Ok(acp::RequestPermissionResponse::new(
            acp::RequestPermissionOutcome::Cancelled,
        )))
        .await
        .unwrap();
    assert!(follower.permission.is_none());
    fixture.idle(&fixture.runtime).await;
    let snapshot = fixture
        .runtime
        .get_session_snapshot(fixture.id.clone())
        .await
        .unwrap();
    assert!(snapshot.pending_script.is_none());
    assert!(matches!(
        snapshot.turn_outcome,
        Some(TurnOutcome::Completed)
    ));
    let texts: Vec<_> = snapshot
        .history
        .values()
        .map(|message| message.display_text().into_owned())
        .collect();
    assert!(texts.iter().any(|text| text.contains("after dismissal")));
    assert!(texts.iter().any(|text| text.contains("status=\"denied\"")));
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    fixture.runtime.shutdown().await.unwrap();
}
#[tokio::test]
async fn cancelling_a_prompt_keeps_other_clients_queued_messages() {
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(vec![
        Reply::Held("own-start", "own-tail", release),
        Reply::Text("queued-reply"),
    ])
    .await;
    fixture.follow();
    let prompt = tokio::spawn({
        let runtime = fixture.runtime.clone();
        let turn = fixture.session.begin_turn().unwrap();
        let request = acp::PromptRequest::new(fixture.id.to_string(), vec![content::text("own")]);
        let connection = fixture.connection();
        async move { crate::prompt::run(&runtime, turn, request, connection).await }
    });
    fixture
        .until_update("agent_message_chunk", "own-start")
        .await;
    assert!(matches!(
        fixture.send(&fixture.runtime, "queued elsewhere").await,
        SubmitMessageOutcome::Queued { .. }
    ));
    fixture.session.cancel();
    let response = prompt.await.unwrap().unwrap();
    assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
    fixture
        .until_update("agent_message_chunk", "queued-reply")
        .await;
    assert_eq!(fixture.updates("user_message_chunk", "queued elsewhere"), 1);
    assert_eq!(fixture.updates("user_message_chunk", "own"), 0);
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn lag_resyncs_missed_output_once_without_ending_the_turn() {
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(vec![Reply::Held("partial", "tail", release.clone())]).await;
    let mut actual = fixture.runtime.subscribe_session(&fixture.id);
    fixture.send(&fixture.runtime, "other").await;
    let (missed, message_id) = loop {
        let event = actual.recv().await.unwrap();
        if let Event::ItemStarted {
            ref message_id,
            item: AssistantItem::Text { ref text, .. },
            ..
        } = event.event
            && text == "partial"
        {
            let message_id = message_id.clone();
            break (event, message_id);
        }
    };
    let delta = |sequence, delta: &str| RuntimeEvent {
        sequence,
        event: Event::ItemDelta {
            session_id: fixture.id.clone(),
            message_id: message_id.clone(),
            item_index: 0,
            delta: delta.into(),
        },
    };
    let (sender, lagged) = broadcast::channel(2);
    sender
        .send(RuntimeEvent {
            sequence: 1,
            event: Event::HistoryUpdated {
                session_id: fixture.id.clone(),
            },
        })
        .unwrap();
    sender.send(missed).unwrap();
    sender.send(delta(2, "stale")).unwrap();
    fixture.follow_with(lagged);
    fixture.until_update("agent_message_chunk", "partial").await;
    sender.send(delta(u64::MAX, "fresh")).unwrap();
    fixture.until_update("agent_message_chunk", "fresh").await;
    assert!(
        fixture
            .runtime
            .get_session_snapshot(fixture.id.clone())
            .await
            .unwrap()
            .session
            .is_running
    );
    release.notify_one();
    fixture.idle(&fixture.runtime).await;
    assert_eq!(fixture.updates("agent_message_chunk", "partial"), 1);
    assert_eq!(fixture.updates("agent_message_chunk", "stale"), 0);
    assert_eq!(fixture.updates("user_message_chunk", "other"), 1);
    fixture.runtime.shutdown().await.unwrap();
}
