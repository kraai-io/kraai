use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::ProviderManager;
use kraai_tool_core::ToolManager;
use kraai_types::{ChatRole, ProviderId, TokenUsage};
use tokio::sync::broadcast;

use super::harness::{
    RetryNotifyingProvider, RuntimeTestHarness, ScriptedChunk, create_session_with_profile,
};
use crate::Event;

#[tokio::test]
async fn provider_retry_observer_is_forwarded_to_runtime_events() -> Result<()> {
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("retry-mock"),
        Box::new(RetryNotifyingProvider {
            id: ProviderId::new("retry-mock"),
        }),
    );

    let Some(harness) = RuntimeTestHarness::new_with_parts(providers, ToolManager::new()).await
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
        std::time::Duration::from_secs(5),
        collect_events(&mut first, &session_id),
    )
    .await
    .map_err(|_| eyre!("timed out waiting for first subscriber events"))??;
    let second_events = tokio::time::timeout(
        std::time::Duration::from_secs(5),
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
            cache_write_tokens: 2,
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
    assert_eq!(usage.usage.used_context_tokens(), 42);

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
    assert_eq!(usage.cache_write_tokens, 2);

    harness.shutdown().await;
    Ok(())
}
