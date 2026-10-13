use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use agent_client_protocol::{Agent, Client, ConnectionTo, Lines};
use axum::{
    Router,
    body::{Body, Bytes},
    routing::{get, post},
};
use color_eyre::eyre::{Result, ensure, eyre};
use futures::{StreamExt, stream};
use kraai_runtime::{RuntimeBuilder, RuntimeEvent, RuntimeStartupState};
use kraai_types::{MessageStatus, SessionId};
use serde_json::json;
use tokio_util::task::AbortOnDropHandle;

use super::*;

#[tokio::test]
async fn own_session_lag_resyncs_without_cancelling_the_turn() -> Result<()> {
    let root = tempfile::tempdir()?;
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server =
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
    .await?;
    let runtime = RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .storage_root(root.path().join("state"))
        .provider_config_path(root.path().join("providers.toml"))
        .mcp_config_path(root.path().join("mcp.toml"))
        .build_on(&tokio::runtime::Handle::current());
    let startup = runtime.wait_for_startup().await?;
    ensure!(
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
    .await?;
    let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = futures::sink::unfold(lines.clone(), async |lines, line: String| {
        if let Ok(mut captured) = lines.lock() {
            captured.push(line);
        }
        Ok::<_, std::io::Error>(lines)
    });
    let transport = Lines::new(sink, stream::pending::<std::io::Result<String>>());
    let (connection_tx, connection_rx) = tokio::sync::oneshot::channel();
    let _protocol = AbortOnDropHandle::new(tokio::spawn(Agent.builder().connect_with(
        transport,
        async move |connection| {
            connection_tx
                .send(connection)
                .map_err(|_connection| error::internal("connection fixture closed"))?;
            futures::future::pending::<agent_client_protocol::Result<()>>().await
        },
    )));
    let inner = connection_rx.await?;
    tokio::time::timeout(
        Duration::from_secs(10),
        check_lag(runtime, id, session, inner, requests, lines),
    )
    .await?
}

async fn check_lag(
    runtime: kraai_runtime::RuntimeHandle,
    id: SessionId,
    session: Arc<crate::session::Session>,
    inner: ConnectionTo<Client>,
    requests: Arc<AtomicUsize>,
    lines: Arc<std::sync::Mutex<Vec<String>>>,
) -> Result<()> {
    let output = Output::new(
        id.clone(),
        Connection::new(inner.clone(), crate::Stdio::new().budget),
    );
    let turn = session.begin_turn()?;
    let mut actual_events = runtime.subscribe_session(&id);
    runtime
        .send_message(
            id.clone(),
            "first".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    let partial_id = loop {
        if let Event::ItemStarted {
            message_id,
            item: kraai_types::AssistantItem::Text { text, .. },
            ..
        } = actual_events.recv().await?.event
            && text == "partial"
        {
            break message_id;
        }
    };
    let (sender, mut lagged) = broadcast::channel(1);
    for sequence in 1..=2 {
        sender.send(RuntimeEvent {
            sequence,
            event: Event::HistoryUpdated {
                session_id: id.clone(),
            },
        })?;
    }
    let stop_reason = {
        let prompt = finish_prompt(&runtime, &mut lagged, output, &turn.token);
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
                .await?
                .session
                .is_running
        );
        turn.token.cancel();
        prompt.await?.stop_reason
    };
    assert_eq!(stop_reason, acp::StopReason::Cancelled);
    drop(sender);
    drop(turn);
    let partial_lines = lines
        .lock()
        .map_err(|error| eyre!("{error}"))?
        .iter()
        .filter(|line| line.contains("agent_message_chunk") && line.contains("\"partial\""))
        .count();
    assert_eq!(
        partial_lines, 1,
        "resync must send missed text exactly once"
    );
    let snapshot = runtime.get_session_snapshot(id.clone()).await?;
    assert!(!snapshot.session.is_running);
    let partial = snapshot
        .history
        .get(&kraai_types::MessageId::new(partial_id.clone()))
        .ok_or_else(|| eyre!("cancelled partial response was not persisted"))?;
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
        &runtime,
        session.begin_turn()?,
        acp::PromptRequest::new(id.to_string(), vec![content::text("second")]),
        Connection::new(inner, crate::Stdio::new().budget),
    )
    .await?;
    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    let snapshot = runtime.get_session_snapshot(id).await?;
    assert!(!snapshot.session.is_running);
    assert!(
        snapshot
            .history
            .values()
            .any(|message| message.status == MessageStatus::Complete
                && message.display_text() == "after overflow")
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    runtime.shutdown().await?;
    Ok(())
}
