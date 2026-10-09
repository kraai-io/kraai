use std::hint::black_box;

use color_eyre::eyre::{OptionExt, Result, ensure};
use kraai_persistence::{
    AppendMessageRequest, ConversationSnapshot, Persistence, SessionMeta, SessionStore,
};
use kraai_types::{ConversationItem, MessageId, MessageStatus};

use crate::benchmark::{Benchmark, Context};

const MESSAGES: u64 = 128;
const READS: usize = 16;
const MESSAGE_BYTES: usize = 8_192;
const SESSION: &str = "performance-history";

pub(super) struct Case;

pub(super) struct Fixture {
    persistence: Option<Persistence>,
    payload: String,
}

pub(super) struct Output {
    ids: Vec<MessageId>,
    previous_tips_match: bool,
    snapshot: ConversationSnapshot,
}

impl Benchmark for Case {
    type Fixture = Fixture;
    type Output = Output;

    const NAME: &str = "persistence-history";
    const OPERATIONS: u64 = MESSAGES;
    const DESCRIPTION: &str = "Append 128 messages of 8 KiB, then reopen and read history 16 times";

    async fn setup(context: &Context) -> Result<Self::Fixture> {
        let persistence = Persistence::open(&context.directory).await?;
        persistence
            .sessions()
            .save(&SessionMeta {
                revision: 0,
                id: SESSION.into(),
                tip_id: None,
                workspace_dir: context.directory.clone(),
                created_at: 1,
                updated_at: 1,
                title: None,
                selected_profile_id: None,
                selected_model: None,
            })
            .await?;
        Ok(Fixture {
            persistence: Some(persistence),
            payload: "0123456789abcde\n".repeat(MESSAGE_BYTES / 16),
        })
    }

    async fn run(context: &Context, fixture: &mut Self::Fixture) -> Result<Self::Output> {
        let persistence = fixture
            .persistence
            .as_ref()
            .ok_or_eyre("Persistence fixture was already consumed")?;
        let mut ids = Vec::new();
        let mut previous_tips_match = true;
        for _ in 0..MESSAGES {
            let appended = persistence
                .conversations()
                .append_message(AppendMessageRequest {
                    session_id: SESSION.into(),
                    content: ConversationItem::User {
                        content: fixture.payload.clone().into(),
                    },
                    status: MessageStatus::Complete,
                    agent_profile_id: None,
                    generation: None,
                    title_if_first_message: Some("Performance history".into()),
                })
                .await?;
            previous_tips_match &= appended.previous_tip.as_ref() == ids.last();
            ids.push(appended.message.id);
        }
        drop(fixture.persistence.take());
        let mut last_snapshot = None;
        for read in 0..READS {
            let reopened = Persistence::open(&context.directory).await?;
            let snapshot = black_box(
                reopened
                    .messages()
                    .read_conversation(SESSION)
                    .await?
                    .ok_or_eyre("Persisted conversation disappeared")?,
            );
            if read + 1 == READS {
                last_snapshot = Some(snapshot);
            }
        }
        Ok(Output {
            ids,
            previous_tips_match,
            snapshot: last_snapshot.ok_or_eyre("History was not read")?,
        })
    }

    async fn verify(
        _context: &Context,
        fixture: Self::Fixture,
        output: Self::Output,
    ) -> Result<()> {
        let Output {
            ids,
            previous_tips_match,
            snapshot,
        } = output;
        ensure!(previous_tips_match, "Append lost the previous tip");
        ensure!(ids.len() as u64 == MESSAGES, "Append omitted messages");
        ensure!(
            snapshot.history.len() as u64 == MESSAGES,
            "History lost messages"
        );
        ensure!(snapshot.session.id == SESSION, "History session changed");
        ensure!(
            snapshot.session.tip_id.as_ref() == ids.last(),
            "History tip changed"
        );
        ensure!(
            snapshot.session.title.as_deref() == Some("Performance history"),
            "History title changed"
        );
        let expected_content = ConversationItem::User {
            content: fixture.payload.into(),
        };
        let mut previous = None;
        for id in ids {
            let stored = snapshot
                .history
                .get(&id)
                .ok_or_eyre("Persisted message disappeared")?;
            ensure!(stored.parent_id == previous, "History ancestry changed");
            ensure!(
                stored.content == expected_content,
                "Message content changed"
            );
            ensure!(
                stored.status == MessageStatus::Complete,
                "Message status changed"
            );
            previous = Some(id);
        }
        Ok(())
    }
}
