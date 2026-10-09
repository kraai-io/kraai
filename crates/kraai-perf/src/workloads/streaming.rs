use std::hint::black_box;

use color_eyre::eyre::{OptionExt, Result, bail, ensure};
use futures::StreamExt;
use kraai_agent::AgentManager;
use kraai_persistence::Persistence;
use kraai_provider_core::{ProviderManager, ProviderRequestContext, ProviderStreamEvent};
use kraai_types::{MessageId, MessageStatus};

use crate::benchmark::{Benchmark, Context};
use crate::fixtures::{agent, provider};

const TURNS: usize = 4;
const SNAPSHOT_INTERVAL: usize = 128;

pub(super) struct Case;

pub(super) struct Fixture {
    manager: AgentManager,
    persistence: Persistence,
    providers: ProviderManager,
    session: String,
    expected: String,
}

pub(super) struct Reply {
    id: MessageId,
    chunks: usize,
    visible_text_matches: bool,
    completed_session: Option<String>,
    lease_released: bool,
}

impl Benchmark for Case {
    type Fixture = Fixture;
    type Output = Vec<Reply>;

    const NAME: &str = "streaming-response";
    const OPERATIONS: u64 = (TURNS * provider::CHUNKS) as u64;
    const DESCRIPTION: &str = "Stream four 256 KiB replies in 128-byte chunks, persist 64 snapshots, and complete four turns";
    const FIXTURES: &[&[u8]] = agent::SOURCES;

    async fn setup(context: &Context) -> Result<Self::Fixture> {
        agent::prepare(&context.directory)?;
        let (mut manager, persistence) = agent::create(&context.directory).await?;
        let session = manager
            .create_session_with(None, Some(agent::PROFILE.into()))
            .await?;
        let providers = manager.cloned_provider_manager();
        let expected: String = (0..provider::CHUNKS).map(provider::chunk).collect();
        ensure!(
            expected.len() == provider::CHUNKS * provider::CHUNK_BYTES,
            "Fixture chunk size changed"
        );
        Ok(Fixture {
            manager,
            persistence,
            providers,
            session,
            expected,
        })
    }

    async fn run(_context: &Context, fixture: &mut Self::Fixture) -> Result<Self::Output> {
        let Fixture {
            manager,
            persistence,
            providers,
            session,
            ..
        } = fixture;
        let mut replies = Vec::with_capacity(TURNS);
        for turn in 0..TURNS {
            persistence.sessions().claim_turn(session).await?;
            let request = manager
                .prepare_start_stream(
                    session,
                    format!("Fixture turn {turn}").into(),
                    provider::model_id(),
                    provider::provider_id(),
                    Default::default(),
                )
                .await?;
            let message_id = request.message_id;
            let mut stream = providers
                .generate_reply_stream(
                    request.provider_id,
                    &request.model_id,
                    request.provider_request,
                    ProviderRequestContext::default(),
                )
                .await?;
            let mut chunks = 0;
            let mut visible_text_matches = true;
            while let Some(event) = stream.next().await {
                let ProviderStreamEvent::TextDelta {
                    item_id,
                    phase,
                    delta,
                } = event?
                else {
                    bail!("Unexpected event in the fixed response stream");
                };
                let visible = black_box(
                    manager
                        .append_text_chunk(&message_id, &item_id, phase, black_box(&delta))
                        .await
                        .ok_or_eyre("Streamed chunk was rejected")?,
                );
                visible_text_matches &= visible == delta;
                chunks += 1;
                if chunks % SNAPSHOT_INTERVAL == 0 {
                    manager.publish_streaming_snapshots().await?;
                }
            }
            let completed_session = manager.complete_message(&message_id).await?;
            manager.clear_active_turn(session);
            let lease_released = persistence.sessions().release_turn(session).await?;
            replies.push(Reply {
                id: message_id,
                chunks,
                visible_text_matches,
                completed_session,
                lease_released,
            });
        }
        Ok(replies)
    }

    async fn verify(
        context: &Context,
        fixture: Self::Fixture,
        replies: Self::Output,
    ) -> Result<()> {
        let Fixture {
            manager,
            persistence,
            providers,
            session,
            expected,
        } = fixture;
        ensure!(replies.len() == TURNS, "Streaming omitted turns");
        for reply in &replies {
            ensure!(
                reply.chunks == provider::CHUNKS,
                "Provider omitted response chunks"
            );
            ensure!(reply.visible_text_matches, "Streamed text changed");
            ensure!(
                reply.completed_session.as_deref() == Some(session.as_str()),
                "Stream completion lost its session"
            );
            ensure!(reply.lease_released, "Turn lease was not released");
        }
        drop((manager, persistence, providers));
        let reopened = Persistence::open(&context.directory.join("storage")).await?;
        let snapshot = reopened
            .messages()
            .read_conversation(&session)
            .await?
            .ok_or_eyre("Streamed conversation disappeared after reopen")?;
        ensure!(
            snapshot.history.len() == TURNS * 2,
            "Reopened history lost messages"
        );
        ensure!(
            snapshot.session.tip_id.as_ref() == replies.last().map(|reply| &reply.id),
            "Reopened history tip changed"
        );
        for reply in replies {
            let saved = snapshot
                .history
                .get(&reply.id)
                .ok_or_eyre("Persisted response disappeared")?;
            ensure!(
                saved.content.display_text() == expected,
                "Persisted response text changed"
            );
            ensure!(
                saved.status == MessageStatus::Complete,
                "Persisted response status changed"
            );
        }
        Ok(())
    }
}
