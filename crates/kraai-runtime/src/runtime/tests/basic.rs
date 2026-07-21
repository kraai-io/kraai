use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::ProviderManager;
use kraai_types::{ChatRole, ProviderId, TokenUsage};
use tokio::sync::broadcast;

use super::harness::{
    RetryNotifyingProvider, RuntimeTestHarness, ScriptedChunk, create_session_with_profile,
};
use crate::handle::{Command, RuntimeLifecycle};
use crate::{Event, RuntimeHandle, RuntimeStartupState};

#[test]
fn idle_config_watcher_does_not_block_single_thread_runtime() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let Some(harness) = RuntimeTestHarness::new(Vec::new()).await else {
            return Ok(());
        };

        tokio::time::timeout(Duration::from_secs(1), harness.handle.list_sessions()).await??;

        harness.shutdown().await;
        Ok(())
    })
}

#[tokio::test]
async fn runtime_shutdown_is_awaitable_and_rejects_new_commands() -> Result<()> {
    for _ in 0..2 {
        let Some(harness) = RuntimeTestHarness::new(Vec::new()).await else {
            return Ok(());
        };
        let handle = harness.handle.clone();

        tokio::time::timeout(Duration::from_secs(1), handle.shutdown()).await??;
        assert!(handle.list_sessions().await.is_err());

        tokio::time::timeout(Duration::from_secs(1), harness.shutdown()).await?;
    }
    Ok(())
}

#[tokio::test]
async fn startup_state_remains_observable_after_initial_result() -> Result<()> {
    let (command_tx, _command_rx) = tokio::sync::mpsc::channel(1);
    let (event_tx, _) = tokio::sync::broadcast::channel(1);
    let (startup_tx, startup_rx) = tokio::sync::watch::channel(RuntimeStartupState::Starting);
    startup_tx.send_replace(RuntimeStartupState::Failed(String::from("initial failure")));
    let handle = RuntimeHandle {
        command_tx,
        event_tx,
        lifecycle: None,
        startup_rx,
    };

    tokio::task::yield_now().await;

    assert_eq!(
        handle.startup_status(),
        RuntimeStartupState::Failed(String::from("initial failure"))
    );
    assert_eq!(
        handle.wait_for_startup().await?,
        RuntimeStartupState::Failed(String::from("initial failure"))
    );
    Ok(())
}

#[tokio::test]
async fn dropping_last_handle_signals_shutdown_when_command_queue_is_full() {
    let (command_tx, _command_rx) = tokio::sync::mpsc::channel(1);
    let (event_tx, _) = tokio::sync::broadcast::channel(1);
    let (_startup_tx, startup_rx) = tokio::sync::watch::channel(RuntimeStartupState::Starting);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    command_tx
        .try_send(Command::LoadConfig)
        .expect("fill command queue");
    let handle = RuntimeHandle {
        command_tx,
        event_tx,
        lifecycle: Some(std::sync::Arc::new(RuntimeLifecycle::new(shutdown_tx))),
        startup_rx,
    };

    drop(handle);

    assert!(*shutdown_rx.borrow());
}

#[tokio::test]
async fn provider_retry_observer_is_forwarded_to_runtime_events() -> Result<()> {
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("retry-mock"),
        Box::new(RetryNotifyingProvider {
            id: ProviderId::new("retry-mock"),
        }),
    );

    let Some(harness) = RuntimeTestHarness::new_with_parts(providers).await else {
        return Ok(());
    };
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("retry-mock"),
        )
        .await?;

    let events = harness
        .events
        .wait_for("provider retry event", |events| {
            events.iter().any(|event| {
                matches!(event, Event::ProviderRetryScheduled { session_id: event_session, .. } if event_session == &session_id)
            })
        })
        .await;

    let retry_event = events.iter().find_map(|event| match event {
        Event::ProviderRetryScheduled {
            session_id: event_session,
            provider_id,
            model_id,
            operation,
            retry_number,
            delay_seconds,
            reason,
        } if event_session == &session_id => Some((
            provider_id.clone(),
            model_id.clone(),
            operation.clone(),
            *retry_number,
            *delay_seconds,
            reason.clone(),
        )),
        _ => None,
    });

    assert_eq!(
        retry_event,
        Some((
            String::from("retry-mock"),
            String::from("mock-model"),
            String::from("responses"),
            1,
            1,
            String::from("HTTP 429"),
        ))
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn runtime_broadcasts_events_to_multiple_subscribers() -> Result<()> {
    let Some(harness) =
        RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("shared event stream")]]).await
    else {
        return Ok(());
    };
    let mut first = harness.handle.subscribe();
    let mut second = harness.handle.subscribe();

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    async fn collect_events(
        receiver: &mut broadcast::Receiver<Event>,
        session_id: &str,
    ) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let is_complete = matches!(
                        &event,
                        Event::StreamComplete {
                            session_id: completed_session,
                            ..
                        } if completed_session == session_id
                    );
                    events.push(event);
                    if is_complete {
                        return Ok(events);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(eyre!("event stream closed before completion"));
                }
            }
        }
    }

    let first_events = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        collect_events(&mut first, &session_id),
    )
    .await
    .map_err(|_| eyre!("timed out waiting for first subscriber events"))??;
    let second_events = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        collect_events(&mut second, &session_id),
    )
    .await
    .map_err(|_| eyre!("timed out waiting for second subscriber events"))??;

    for events in [&first_events, &second_events] {
        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::StreamStart {
                    session_id: started_session,
                    ..
                } if started_session == &session_id
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::StreamChunk {
                    session_id: chunk_session,
                    chunk,
                    ..
                } if chunk_session == &session_id && chunk == "shared event stream"
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                Event::StreamComplete {
                    session_id: completed_session,
                    ..
                } if completed_session == &session_id
            )
        }));
    }

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn completed_stream_persists_context_usage_for_latest_assistant_turn() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain("usage-aware reply"),
        ScriptedChunk::usage(TokenUsage {
            total_tokens: 42,
            input_tokens: 10,
            output_tokens: 20,
            reasoning_tokens: 3,
            cache_read_tokens: 7,
        }),
    ]])
    .await
    else {
        return Ok(());
    };

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("stream completion", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::StreamComplete {
                        session_id: completed_session,
                        ..
                    } if completed_session == &session_id
                )
            })
        })
        .await;

    let usage = harness
        .handle
        .get_session_context_usage(session_id.clone())
        .await?
        .expect("context usage should be available");

    assert_eq!(usage.provider_id, "mock");
    assert_eq!(usage.model_id, "mock-model");
    assert_eq!(usage.usage.used_context_tokens(), 40);

    let history = harness.handle.get_chat_history(session_id).await?;
    let assistant = history
        .values()
        .find(|message| message.role == ChatRole::Assistant)
        .expect("assistant message should be present");
    let generation = assistant
        .generation
        .as_ref()
        .expect("assistant generation metadata should be persisted");
    let usage = generation
        .usage
        .as_ref()
        .expect("assistant usage should be persisted");
    assert_eq!(usage.total_tokens, 42);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn invalid_public_ids_return_errors_without_stopping_runtime() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new(Vec::new()).await else {
        return Ok(());
    };

    let model_error = harness
        .handle
        .send_message(
            String::from("session"),
            String::from("message"),
            String::new(),
            String::from("provider"),
        )
        .await
        .unwrap_err();
    assert!(model_error.to_string().contains("model_id"));

    let provider_error = harness
        .handle
        .send_message(
            String::from("session"),
            String::from("message"),
            String::from("model"),
            String::new(),
        )
        .await
        .unwrap_err();
    assert!(provider_error.to_string().contains("provider_id"));

    let approve_error = harness
        .handle
        .approve_script(String::from("session"), String::new())
        .await
        .unwrap_err();
    assert!(approve_error.to_string().contains("execution_id"));
    let deny_error = harness
        .handle
        .deny_script(String::from("session"), String::new())
        .await
        .unwrap_err();
    assert!(deny_error.to_string().contains("execution_id"));

    tokio::time::timeout(Duration::from_secs(1), harness.handle.list_sessions()).await??;
    harness.shutdown().await;
    Ok(())
}
