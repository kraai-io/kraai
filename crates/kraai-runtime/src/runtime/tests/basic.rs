use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::ProviderManager;
use kraai_test_support::hang_guard;
use kraai_types::{ChatRole, ProviderId, SessionId, TokenUsage};
use tokio::sync::broadcast;

use super::harness::FullHistory as _;
use super::harness::{
    RetryNotifyingProvider, RuntimeTestHarness, ScriptedChunk, create_session_with_profile,
};
use crate::{Event, RuntimeErrorKind, RuntimeEvent};

#[test]
fn idle_config_watcher_does_not_block_single_thread_runtime() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let harness = RuntimeTestHarness::new(Vec::new()).await;

        hang_guard(harness.handle.list_sessions()).await??;

        harness.shutdown().await;
        Ok(())
    })
}

#[tokio::test]
async fn profile_queries_share_reads_and_validate_workspace_before_locking() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await;
    let session_id = harness
        .handle
        .create_session_with(crate::CreateSessionRequest {
            workspace_dir: None,
            profile_id: Some(String::from("test-profile")),
        })
        .await?;
    let profiles = hang_guard(harness.handle.list_agent_profiles(session_id)).await;
    let profiles = profiles??;
    assert_eq!(
        profiles.selected_profile_id.as_deref(),
        Some("test-profile")
    );

    let catalog = harness.handle.get_agent_profile_catalog(None).await?;
    assert_eq!(catalog.default_profile_id, "test-profile");
    assert_eq!(catalog.profiles, profiles.profiles);
    assert_eq!(catalog.warnings, profiles.warnings);
    assert_eq!(
        catalog.workspace_dir,
        harness.data_dir.join("workspace").display().to_string()
    );

    let missing = harness.data_dir.join("missing-workspace");
    let invalid = hang_guard(
        harness
            .handle
            .get_agent_profile_catalog(Some(missing.display().to_string())),
    )
    .await;
    let error = invalid?
        .err()
        .ok_or_else(|| eyre!("missing workspace unexpectedly returned a profile catalog"))?;
    assert_eq!(error.kind, RuntimeErrorKind::InvalidArgument);
    assert_eq!(
        error.message,
        format!("Workspace directory does not exist: {}", missing.display())
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn runtime_shutdown_is_awaitable_and_rejects_new_commands() -> Result<()> {
    for _ in 0..2 {
        let harness = RuntimeTestHarness::new(Vec::new()).await;
        let handle = harness.handle.clone();

        hang_guard(handle.shutdown()).await??;
        assert!(handle.list_sessions().await.is_err());

        hang_guard(harness.shutdown()).await?;
    }
    Ok(())
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

    let harness = RuntimeTestHarness::new_with_parts(providers).await;
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("retry-mock"),
            Default::default(),
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
    let harness =
        RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("shared event stream")]]).await;
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
            Default::default(),
        )
        .await?;

    async fn collect_events(
        receiver: &mut broadcast::Receiver<RuntimeEvent>,
        session_id: &SessionId,
    ) -> Result<Vec<RuntimeEvent>> {
        let mut events = Vec::new();
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let is_complete = matches!(
                        &event.event,
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

    let first_events = hang_guard(collect_events(&mut first, &session_id))
        .await
        .map_err(|_| eyre!("timed out waiting for first subscriber events"))??;
    let second_events = hang_guard(collect_events(&mut second, &session_id))
        .await
        .map_err(|_| eyre!("timed out waiting for second subscriber events"))??;

    for events in [&first_events, &second_events] {
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                Event::StreamStart {
                    session_id: started_session,
                    ..
                } if started_session == &session_id
            )
        }));
        assert!(events.iter().any(|event| {
            super::harness::streamed_text(&event.event, &session_id) == Some("shared event stream")
        }));
        assert!(events.iter().any(|event| {
            matches!(
                &event.event,
                Event::StreamComplete {
                    session_id: completed_session,
                    ..
                } if completed_session == &session_id
            )
        }));
    }

    let snapshot = harness.handle.get_session_snapshot(session_id).await?;
    assert!(snapshot.turn_timer.last_duration().is_some());
    assert!(
        snapshot
            .turn_timer
            .elapsed(std::time::Instant::now())
            .is_none()
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn completed_stream_persists_context_usage_for_latest_assistant_turn() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![vec![
        ScriptedChunk::plain("usage-aware reply"),
        ScriptedChunk::usage(TokenUsage {
            total_tokens: 42,
            input_tokens: 10,
            output_tokens: 20,
            reasoning_tokens: 3,
            cache_read_tokens: 7,
            ..Default::default()
        }),
    ]])
    .await;

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
            Default::default(),
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

    let events = harness.events.snapshot();
    let receipt_states = events
        .iter()
        .filter_map(|event| match event {
            Event::RequestUsageUpdated {
                session_id: updated_session,
                request,
            } if updated_session == &session_id => Some(request.usage.is_some()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(receipt_states, vec![false, true]);
    let completed = events.iter().position(|event| matches!(event, Event::StreamComplete { session_id: completed_session, .. } if completed_session == &session_id)).expect("stream completion");
    let priced = events.iter().position(|event| matches!(event, Event::RequestUsageUpdated { request, .. } if request.usage.is_some())).expect("usage update");
    assert!(priced < completed);

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
        .find(|message| message.role() == ChatRole::Assistant)
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
async fn session_list_omits_sessions_without_messages() -> Result<()> {
    let harness = RuntimeTestHarness::new(vec![vec![ScriptedChunk::plain("first reply")]]).await;

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    assert!(harness.handle.list_sessions().await?.is_empty());
    assert!(
        harness
            .handle
            .get_workspace_state(session_id.clone())
            .await?
            .is_some()
    );

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("hello"),
            String::from("mock-model"),
            String::from("mock"),
            Default::default(),
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

    let listed = harness.handle.list_sessions().await?;
    assert_eq!(
        listed.iter().map(|session| &session.id).collect::<Vec<_>>(),
        vec![&session_id]
    );

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn domain_failures_preserve_their_public_error_kind() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await;

    let missing_session = harness
        .handle
        .get_session_snapshot(SessionId::new("missing-session"))
        .await
        .unwrap_err();
    assert_eq!(missing_session.kind, RuntimeErrorKind::NotFound);

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let invalid_workspace = harness
        .handle
        .set_workspace_dir(
            session_id.clone(),
            harness
                .data_dir
                .join("does-not-exist")
                .display()
                .to_string(),
        )
        .await
        .unwrap_err();
    assert_eq!(invalid_workspace.kind, RuntimeErrorKind::InvalidArgument);

    let missing_approval = harness
        .handle
        .approve_script(session_id, ulid::Ulid::generate().to_string())
        .await
        .unwrap_err();
    assert_eq!(missing_approval.kind, RuntimeErrorKind::NotFound);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn session_snapshot_waits_for_state_event_boundary() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await;
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let barrier = harness.runtime.sessions.barrier(&session_id);
    let mutation_guard = barrier.read().await;
    let mut snapshot_task = tokio::spawn({
        let handle = harness.handle.clone();
        let session_id = session_id.clone();
        async move { handle.get_session_snapshot(session_id).await }
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut snapshot_task)
            .await
            .is_err(),
        "snapshot crossed an in-flight state mutation"
    );
    harness
        .runtime
        .send_event(Event::HistoryUpdated { session_id });
    let mutation_sequence = harness.runtime.event_tx.latest_sequence();
    drop(mutation_guard);

    let snapshot = snapshot_task.await??;
    assert!(snapshot.event_sequence >= mutation_sequence);

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn deleting_session_removes_only_its_timer() -> Result<()> {
    let harness = RuntimeTestHarness::new(Vec::new()).await;
    let deleted = create_session_with_profile(&harness.handle, "test-profile").await?;
    let retained = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness.runtime.event_tx.resume_timer(&deleted);
    harness.runtime.event_tx.resume_timer(&retained);
    let retained_timer = harness.runtime.event_tx.turn_snapshot(&retained).timer;
    harness.handle.delete_session(deleted.clone()).await?;
    assert_eq!(
        harness.runtime.event_tx.turn_snapshot(&deleted).timer,
        crate::TurnTimer::default()
    );
    assert_eq!(
        harness.runtime.event_tx.turn_snapshot(&retained).timer,
        retained_timer
    );
    harness.shutdown().await;
    Ok(())
}
