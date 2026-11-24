use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use agent_client_protocol::{Agent, Client, Lines, v1::ConnectionTo};
use axum::{
    Router,
    body::{Body, Bytes},
    routing::{get, post},
};
use futures::{StreamExt, stream};
use kraai_runtime::{RuntimeBuilder, RuntimeEvent, RuntimeStartupState};
use kraai_types::MessageStatus;
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::task::AbortOnDropHandle;

use super::*;

struct Fixture {
    runtime: RuntimeHandle,
    id: SessionId,
    session: Arc<crate::session::Session>,
    inner: ConnectionTo<Client>,
    lines: Arc<std::sync::Mutex<Vec<String>>>,
    _protocol: AbortOnDropHandle<agent_client_protocol::Result<()>>,
    _server: AbortOnDropHandle<std::io::Result<()>>,
    _root: tempfile::TempDir,
}

impl Fixture {
    async fn new(router: Router) -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            AbortOnDropHandle::new(tokio::spawn(
                async move { axum::serve(listener, router).await },
            ));
        tokio::fs::write(
            root.path().join("providers.toml"),
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
        let runtime = RuntimeBuilder::new(kraai_providers::DefaultProviders)
            .storage_root(root.path().join("state"))
            .provider_config_path(root.path().join("providers.toml"))
            .mcp_config_path(root.path().join("mcp.toml"))
            .build_on(&tokio::runtime::Handle::current());
        let startup = runtime.wait_for_startup().await.unwrap();
        assert!(
            startup == RuntimeStartupState::ready(),
            "Runtime fixture did not become ready: {startup:?}"
        );
        let (id, session) = crate::session::create(
            &runtime,
            &crate::Options {
                options: Vec::new(),
                provider: Some("mock".into()),
                model: Some("mock-model".into()),
                profile: None,
            },
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
            _protocol: protocol,
            _server: server,
            _root: root,
        }
    }

    fn connection(&self) -> Connection {
        Connection::new(self.inner.clone(), crate::Stdio::new().budget)
    }

    fn chunks(&self, text: &str) -> usize {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains("agent_message_chunk") && line.contains(text))
            .count()
    }

    async fn send(&self, text: &str) -> String {
        match self
            .runtime
            .send_message(
                self.id.clone(),
                text.into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await
            .unwrap()
        {
            SubmitMessageOutcome::Started { message_id } => message_id,
            SubmitMessageOutcome::Queued { .. } => panic!("message was queued"),
        }
    }
}

fn sse(chunk: &serde_json::Value, done: bool) -> Bytes {
    let done = if done { "data: [DONE]\n\n" } else { "" };
    Bytes::from(format!("data: {chunk}\n\n{done}"))
}

fn text_chunk(text: &str, last: bool) -> serde_json::Value {
    json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":if last {Some("stop")} else {None}}]})
}

fn other_client_router(requests: Arc<AtomicUsize>, release: Arc<Notify>) -> Router {
    Router::new()
        .route(
            "/models",
            get(async || axum::Json(json!({"data":[{"id":"mock-model"}]}))),
        )
        .route(
            "/chat/completions",
            post(move || {
                let first = requests.fetch_add(1, Ordering::SeqCst) == 0;
                let release = release.clone();
                async move {
                    let body = if first {
                        stream::iter([Ok(sse(&text_chunk("other-start", false), false))])
                            .chain(stream::once(async move {
                                release.notified().await;
                                Ok::<_, std::io::Error>(sse(&text_chunk("other-tail", true), true))
                            }))
                            .boxed()
                    } else {
                        stream::iter([Ok(sse(&text_chunk("own-reply", true), true))]).boxed()
                    };
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(body),
                    )
                }
            }),
        )
}

async fn other_turn_streaming(
    fixture: &Fixture,
) -> broadcast::Receiver<kraai_runtime::RuntimeEvent> {
    let mut events = fixture.runtime.subscribe_session(&fixture.id);
    fixture.send("other").await;
    loop {
        if let Event::ItemStarted {
            item: AssistantItem::Text { text, .. },
            ..
        } = events.recv().await.unwrap().event
            && text == "other-start"
        {
            return events;
        }
    }
}

#[tokio::test]
async fn prompt_waits_for_another_clients_turn_and_reports_only_its_own() {
    let requests = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(other_client_router(requests.clone(), release.clone())).await;
    kraai_test_support::hang_guard(async {
        let _events = other_turn_streaming(&fixture).await;
        let runtime = fixture.runtime.clone();
        let turn = fixture.session.begin_turn().unwrap();
        let request = acp::PromptRequest::new(fixture.id.to_string(), vec![content::text("own")]);
        let connection = fixture.connection();
        let mut prompt =
            tokio::spawn(async move { run(&runtime, turn, request, connection).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut prompt)
                .await
                .is_err(),
            "prompt ended while another client's turn was running"
        );
        let snapshot = fixture
            .runtime
            .get_session_snapshot(fixture.id.clone())
            .await
            .unwrap();
        assert_eq!(snapshot.queued_messages, 0);
        release.notify_one();
        let response = prompt.await.unwrap().unwrap();
        assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
        assert_eq!(fixture.chunks("own-reply"), 1);
        assert_eq!(fixture.chunks("other-tail"), 0);
        assert_eq!(fixture.chunks("other-start"), 0);
        let snapshot = fixture
            .runtime
            .get_session_snapshot(fixture.id.clone())
            .await
            .unwrap();
        assert!(!snapshot.session.is_running);
        assert!(
            snapshot
                .history
                .values()
                .any(|message| message.status == MessageStatus::Complete
                    && message.display_text() == "own-reply")
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    })
    .await
    .unwrap();
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_a_waiting_prompt_leaves_the_other_turn_running() {
    let requests = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let fixture = Fixture::new(other_client_router(requests.clone(), release.clone())).await;
    kraai_test_support::hang_guard(async {
        let mut events = other_turn_streaming(&fixture).await;
        let runtime = fixture.runtime.clone();
        let session = fixture.session.clone();
        let request = acp::PromptRequest::new(fixture.id.to_string(), vec![content::text("own")]);
        let connection = fixture.connection();
        let mut prompt = tokio::spawn(async move {
            run(&runtime, session.begin_turn().unwrap(), request, connection).await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut prompt)
                .await
                .is_err(),
            "prompt ended while another client's turn was running"
        );
        fixture.session.cancel();
        let response = prompt.await.unwrap().unwrap();
        assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
        let snapshot = fixture
            .runtime
            .get_session_snapshot(fixture.id.clone())
            .await
            .unwrap();
        assert!(snapshot.session.is_running);
        assert_eq!(snapshot.queued_messages, 0);
        release.notify_one();
        loop {
            let event = events.recv().await.unwrap().event;
            assert!(
                !matches!(event, Event::StreamCancelled { .. }),
                "the other client's turn was cancelled"
            );
            if matches!(event, Event::TurnCompleted { .. }) {
                break;
            }
        }
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    })
    .await
    .unwrap();
    fixture.runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn own_session_lag_resyncs_without_cancelling_the_turn() {
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let router = Router::new()
        .route("/models", get(async || axum::Json(json!({"data":[{"id":"mock-model"}]}))))
        .route("/chat/completions", post(move || {
            let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                let chunk = json!({"choices":[{"index":0,"delta":{"content":if first {"partial"} else {"after overflow"}},"finish_reason":if first {None} else {Some("stop")}}]});
                let tail = if first {
                    stream::pending::<std::result::Result<Bytes, std::io::Error>>().boxed()
                } else {
                    stream::iter([Ok(Bytes::from_static(b"data: [DONE]\n\n"))]).boxed()
                };
                ([("content-type", "text/event-stream")], Body::from_stream(stream::iter([Ok(Bytes::from(format!("data: {chunk}\n\n")))]).chain(tail)))
            }
        }));
    let fixture = Fixture::new(router).await;
    kraai_test_support::hang_guard(check_lag(&fixture, requests))
        .await
        .unwrap();
    fixture.runtime.shutdown().await.unwrap();
}

async fn check_lag(fixture: &Fixture, requests: Arc<AtomicUsize>) {
    let Fixture {
        runtime,
        id,
        session,
        ..
    } = fixture;
    let turn = session.begin_turn().unwrap();
    let mut actual_events = runtime.subscribe_session(id);
    let prompt_start = runtime
        .get_tip(id.clone())
        .await
        .unwrap()
        .map(MessageId::new);
    let stream = fixture.send("first").await;
    let output = Output::new(id.clone(), fixture.connection(), prompt_start, stream);
    let (partial_id, partial_item) = loop {
        let event = actual_events.recv().await.unwrap().event;
        if let Event::ItemStarted {
            ref message_id,
            ref item,
            ..
        } = event
            && matches!(item, AssistantItem::Text { text, .. } if text == "partial")
        {
            break (message_id.clone(), event);
        }
    };
    let (sender, mut lagged) = broadcast::channel(1);
    sender
        .send(RuntimeEvent {
            sequence: 1,
            event: Event::HistoryUpdated {
                session_id: id.clone(),
            },
        })
        .unwrap();
    sender
        .send(RuntimeEvent {
            sequence: 2,
            event: partial_item,
        })
        .unwrap();
    let stop_reason = {
        let prompt = finish_prompt(runtime, &mut lagged, output, &turn.token);
        tokio::pin!(prompt);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut prompt)
                .await
                .is_err(),
            "missed events ended the prompt"
        );
        assert!(
            runtime
                .get_session_snapshot(id.clone())
                .await
                .unwrap()
                .session
                .is_running
        );
        turn.token.cancel();
        prompt.await.unwrap().stop_reason
    };
    assert_eq!(stop_reason, acp::StopReason::Cancelled);
    drop(sender);
    drop(turn);
    assert_eq!(
        fixture.chunks("\"partial\""),
        1,
        "resync must send missed text exactly once"
    );
    let snapshot = runtime.get_session_snapshot(id.clone()).await.unwrap();
    assert!(!snapshot.session.is_running);
    let partial = snapshot
        .history
        .get(&MessageId::new(partial_id.clone()))
        .expect("cancelled partial response was not persisted");
    assert_eq!(partial.status, MessageStatus::Complete);
    assert_eq!(partial.display_text(), "partial");
    let mut cancelled = false;
    while let Ok(event) = actual_events.try_recv() {
        cancelled |= matches!(event.event, Event::StreamCancelled { message_id, .. } if message_id == partial_id);
    }
    assert!(
        cancelled,
        "overflow did not cancel the active runtime stream"
    );
    let response = run(
        runtime,
        session.begin_turn().unwrap(),
        acp::PromptRequest::new(id.to_string(), vec![content::text("second")]),
        fixture.connection(),
    )
    .await
    .unwrap();
    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    let snapshot = runtime.get_session_snapshot(id.clone()).await.unwrap();
    assert!(!snapshot.session.is_running);
    assert!(
        snapshot
            .history
            .values()
            .any(|message| message.status == MessageStatus::Complete
                && message.display_text() == "after overflow")
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}
