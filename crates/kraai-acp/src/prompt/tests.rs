use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use agent_client_protocol::{Agent, Lines};
use axum::{
    Router,
    body::{Body, Bytes},
    routing::{get, post},
};
use color_eyre::eyre::{Result, eyre};
use futures::{StreamExt, stream};
use kraai_runtime::{RuntimeBuilder, RuntimeEvent, RuntimeStartupState};
use kraai_types::MessageStatus;
use serde_json::json;
use tokio_util::task::AbortOnDropHandle;

use super::*;

#[tokio::test]
async fn own_session_overflow_cancels_durably_and_accepts_next_prompt() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), check_overflow()).await?
}

async fn check_overflow() -> Result<()> {
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
    let runtime = RuntimeBuilder::new()
        .storage_root(root.path().join("state"))
        .provider_config_path(root.path().join("providers.toml"))
        .mcp_config_path(root.path().join("mcp.toml"))
        .build_on(&tokio::runtime::Handle::current());
    assert_eq!(
        runtime.wait_for_startup().await?,
        RuntimeStartupState::Ready
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
    )
    .await?;
    let sink = futures::sink::unfold((), async |(), _line: String| Ok::<_, std::io::Error>(()));
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
    let connection = Connection::new(inner.clone(), crate::Stdio::new().budget);
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
        if let Event::StreamChunk {
            message_id, chunk, ..
        } = actual_events.recv().await?.event
            && chunk == "partial"
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
    let failure = finish_prompt(
        &runtime,
        &mut lagged,
        Output::new(acp::SessionId::new(id.clone()), connection),
        &turn.token,
    )
    .await;
    let error = failure
        .err()
        .ok_or_else(|| eyre!("overflow unexpectedly succeeded"))?;
    assert!(
        error
            .to_string()
            .contains("Runtime event stream interrupted"),
        "{error}"
    );
    drop(turn);
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
        acp::PromptRequest::new(id.clone(), vec![content::text("second")]),
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
