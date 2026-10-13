use std::collections::VecDeque;
use std::time::Duration;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use kraai_execution_types::{ScriptExecutionId, ScriptExecutionStatus};
use kraai_provider_core::{Provider, ProviderManager, ProviderRequest, ProviderStreamEvent};
use kraai_types::{AssistantPhase, ModelId, ProviderId};
use tokio::sync::{Mutex, mpsc};

use super::harness::FullHistory as _;
use super::harness::{
    RuntimeTestHarness, ScriptedChunk, TEST_TIMEOUT, create_session_with_profile,
};
use crate::{Event, SessionActivity};

pub(super) struct ControlledProvider {
    pub(super) events: Mutex<VecDeque<mpsc::UnboundedReceiver<Result<ProviderStreamEvent>>>>,
}

#[async_trait::async_trait]
impl Provider for ControlledProvider {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("mock")
    }
    async fn list_models(&self) -> Vec<kraai_provider_core::Model> {
        vec![super::harness::mock_model()]
    }
    async fn cache_models(&self) -> Result<()> {
        Ok(())
    }
    async fn register_model(&mut self, _: kraai_provider_core::ModelConfig) -> Result<()> {
        Ok(())
    }
    async fn generate_reply_stream(
        &self,
        _: &ModelId,
        _: ProviderRequest,
        _: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let receiver = self
            .events
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| eyre!("Stream already used"))?;
        Ok(Box::pin(futures::stream::unfold(
            receiver,
            |mut receiver| async move { receiver.recv().await.map(|event| (event, receiver)) },
        )))
    }
}

#[tokio::test]
async fn observers_receive_live_text_reject_busy_submissions_and_claim_after_completion()
-> Result<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([receiver])),
        }),
    );
    let owner = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("owner fixture");
    let observer = RuntimeTestHarness::new_shared(
        vec![vec![ScriptedChunk::plain("second reply")]],
        owner.data_dir.clone(),
    )
    .await
    .expect("observer fixture");
    let session = create_session_with_profile(&owner.handle, "test-profile").await?;
    assert!(observer.handle.load_session(session.clone()).await?);
    assert!(!owner.runtime.session_store.owns_turn(&session).await?);
    assert!(!observer.runtime.session_store.owns_turn(&session).await?);
    owner
        .handle
        .send_message(
            session.clone(),
            "first".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    sender.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "text".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "partial".into(),
    }))?;
    observer.events.wait_for("observer update", |events| events.iter().any(|event| matches!(event, Event::HistoryUpdated { session_id } if session_id == &session))).await;
    let snapshot = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let snapshot = observer
                .handle
                .get_session_snapshot(session.clone())
                .await?;
            if snapshot
                .history
                .values()
                .any(|message| message.display_text().contains("partial"))
            {
                break Ok::<_, crate::RuntimeError>(snapshot);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    assert_eq!(snapshot.activity, SessionActivity::Streaming);
    assert!(snapshot.session.is_running);
    let before = serde_json::to_value(&snapshot.history)?;
    assert!(
        observer
            .handle
            .send_message(
                session.clone(),
                "rejected".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await
            .is_err()
    );
    assert!(
        observer
            .handle
            .undo_last_user_message(session.clone())
            .await
            .is_err()
    );
    assert!(
        observer
            .handle
            .delete_session(session.clone())
            .await
            .is_err()
    );
    assert_eq!(
        serde_json::to_value(observer.handle.get_chat_history(session.clone()).await?)?,
        before
    );
    assert_eq!(
        observer
            .handle
            .get_session_snapshot(session.clone())
            .await?
            .queued_messages,
        0
    );
    assert!(observer.handle.load_session(session.clone()).await?);
    assert!(owner.runtime.session_store.owns_turn(&session).await?);
    drop(sender);
    owner.events.wait_for("owner completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session))).await;
    assert!(!owner.runtime.session_store.owns_turn(&session).await?);
    observer
        .handle
        .send_message(
            session.clone(),
            "second".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    observer.events.wait_for("observer completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session))).await;
    assert!(!observer.runtime.session_store.owns_turn(&session).await?);
    observer.shutdown().await;
    owner.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn queued_turn_claims_ownership_before_starting_the_next_request() -> Result<()> {
    let (first_sender, first_receiver) = mpsc::unbounded_channel();
    let (second_sender, second_receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([first_receiver, second_receiver])),
        }),
    );
    let owner = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("owner fixture");
    let observer = RuntimeTestHarness::new_shared(Vec::new(), owner.data_dir.clone())
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&owner.handle, "test-profile").await?;
    owner
        .handle
        .send_message(
            session.clone(),
            "first".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    assert!(matches!(
        owner
            .handle
            .send_message(
                session.clone(),
                "second".into(),
                "mock-model".into(),
                "mock".into(),
                Default::default(),
            )
            .await?,
        crate::SubmitMessageOutcome::Queued { .. }
    ));
    drop(first_sender);
    second_sender.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "second-text".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "second request is running".into(),
    }))?;
    owner
        .events
        .wait_for("queued request", |events| {
            events.iter().any(|event| {
                super::harness::streamed_text(event, &session) == Some("second request is running")
            })
        })
        .await;
    assert!(owner.runtime.session_store.owns_turn(&session).await?);
    assert!(
        observer
            .runtime
            .session_store
            .claim_turn(&session)
            .await
            .is_err()
    );
    drop(second_sender);
    owner.events.wait_for("queued completion", |events| events.iter().filter(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session)).count() == 2).await;
    observer.shutdown().await;
    owner.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn continuation_during_a_live_stream_preserves_ownership() -> Result<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([receiver])),
        }),
    );
    let owner = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("owner fixture");
    let observer = RuntimeTestHarness::new_shared(Vec::new(), owner.data_dir.clone())
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&owner.handle, "test-profile").await?;
    owner
        .handle
        .send_message(
            session.clone(),
            "first".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    assert_eq!(
        owner
            .handle
            .continue_session(
                session.clone(),
                "mock-model".into(),
                "mock".into(),
                Default::default()
            )
            .await?,
        crate::ContinueSessionOutcome::NothingToContinue
    );
    assert!(owner.runtime.session_store.owns_turn(&session).await?);
    assert!(
        observer
            .runtime
            .session_store
            .claim_turn(&session)
            .await
            .is_err()
    );
    assert!(owner.runtime.agent_manager.is_turn_active(&session));
    drop(sender);
    owner.events.wait_for("owner completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id } if session_id == &session))).await;
    observer.shutdown().await;
    owner.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn failed_intercepted_continuation_releases_ownership_and_preserves_input() -> Result<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([receiver])),
        }),
    );
    let owner = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("owner fixture");
    let observer = RuntimeTestHarness::new_shared(Vec::new(), owner.data_dir.clone())
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&owner.handle, "test-profile").await?;
    owner
        .handle
        .send_message(
            session.clone(),
            "first".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    owner
        .handle
        .send_message(
            session.clone(),
            "preserve this message".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    tokio::fs::write(owner.data_dir.join("AGENTS.md"), [0xff]).await?;
    sender.send(Ok(ProviderStreamEvent::ScriptCall {
        call_id: kraai_types::ToolCallId::new("call"),
        name: "kraai_nushell".into(),
        input: "# timeout=30sec permissions=workspace-write\nprint hello".into(),
    }))?;
    drop(sender);
    owner.events.wait_for("approval", |events| events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id, .. } if session_id == &session))).await;
    let pending = owner
        .handle
        .get_pending_script(session.clone())
        .await?
        .expect("pending approval");
    owner
        .handle
        .deny_script(session.clone(), pending.execution_id)
        .await?;
    owner.events.wait_for("continuation failure", |events| events.iter().any(|event| matches!(event, Event::ContinuationFailed { session_id, .. } if session_id == &session))).await;
    assert!(!owner.runtime.session_store.owns_turn(&session).await?);
    let snapshot = owner.handle.get_session_snapshot(session.clone()).await?;
    assert!(!snapshot.session.is_running);
    assert!(!snapshot.session.profile_locked);
    assert_eq!(snapshot.queued_messages, 1);
    assert!(
        !snapshot
            .history
            .values()
            .any(|message| message.display_text() == "preserve this message")
    );
    observer.runtime.session_store.claim_turn(&session).await?;
    observer
        .runtime
        .session_store
        .release_turn(&session)
        .await?;
    observer.shutdown().await;
    owner.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn delayed_lease_cancellation_does_not_cancel_a_replacement_turn() -> Result<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("mock"),
        Box::new(ControlledProvider {
            events: Mutex::new(VecDeque::from([receiver])),
        }),
    );
    let owner = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("owner fixture");
    let session = create_session_with_profile(&owner.handle, "test-profile").await?;
    let store = &owner.runtime.session_store;
    store.claim_turn(&session).await?;
    let (_, stale_token) = store
        .owned_turns()
        .into_iter()
        .find(|(id, _)| id == &session)
        .expect("owned session");
    store.release_turn(&session).await?;
    owner
        .handle
        .send_message(
            session.clone(),
            "replacement turn".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    assert!(!super::harness::cancel_if_lease_matches(&owner.runtime, &session, stale_token).await?);
    assert!(owner.runtime.session_store.owns_turn(&session).await?);
    assert!(owner.runtime.agent_manager.is_turn_active(&session));
    drop(sender);
    owner.events.wait_for("replacement completion", |events| events.iter().any(|event| matches!(event, Event::TurnCompleted { session_id, .. } if session_id == &session))).await;
    owner.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn heartbeat_renews_during_approval_and_running_scripts_without_the_manager_lock()
-> Result<()> {
    let owner = RuntimeTestHarness::new(vec![vec![ScriptedChunk::native_call(
        "call",
        "# timeout=30sec permissions=workspace-write\nprint hello",
    )]])
    .await
    .expect("owner fixture");
    let observer = RuntimeTestHarness::new_shared(Vec::new(), owner.data_dir.clone())
        .await
        .expect("observer fixture");
    let session = create_session_with_profile(&owner.handle, "test-profile").await?;
    owner
        .handle
        .send_message(
            session.clone(),
            "hello".into(),
            "mock-model".into(),
            "mock".into(),
            Default::default(),
        )
        .await?;
    owner.events.wait_for("approval", |events| events.iter().any(|event| matches!(event, Event::ScriptApprovalRequested { session_id, .. } if session_id == &session))).await;
    let snapshot = observer
        .handle
        .get_session_snapshot(session.clone())
        .await?;
    assert_eq!(snapshot.activity, SessionActivity::AwaitingApproval);
    assert!(snapshot.profiles.profile_locked);
    let pending = owner
        .handle
        .get_pending_script(session.clone())
        .await?
        .expect("approval");
    assert!(
        observer
            .handle
            .approve_script(session.clone(), pending.execution_id.clone())
            .await
            .is_err()
    );
    for running in [false, true] {
        if running {
            owner
                .runtime
                .execution_store
                .mark_running(&ScriptExecutionId::new(&pending.execution_id))
                .await?;
        }
        let before = owner
            .runtime
            .session_store
            .observe(&session)
            .await?
            .expect("lease");
        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                let after = owner
                    .runtime
                    .session_store
                    .observe(&session)
                    .await?
                    .expect("lease");
                if after.lease_expires_at > before.lease_expires_at {
                    break Ok::<_, color_eyre::Report>(());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;
        assert!(
            observer
                .runtime
                .session_store
                .claim_turn(&session)
                .await
                .is_err()
        );
    }
    super::harness::take_pending_approval(&owner.runtime, &session).await;
    owner
        .runtime
        .execution_store
        .finish(
            &ScriptExecutionId::new(&pending.execution_id),
            kraai_persistence::ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Cancelled,
                exit_code: None,
                sandbox_denied: false,
                error: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        )
        .await?;
    owner.runtime.finish_turn(&session).await;
    observer.shutdown().await;
    owner.shutdown().await;
    Ok(())
}
