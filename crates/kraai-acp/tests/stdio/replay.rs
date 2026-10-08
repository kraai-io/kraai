use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{Persistence, SessionStore};
use kraai_types::{
    AssistantItem, AssistantPhase, ConversationItem, Message, MessageId, MessageStatus,
};
use serde_json::{Value, json};

use crate::support::Harness;

#[tokio::test]
async fn reading_client_loads_history_larger_than_output_capacity() -> Result<()> {
    const MESSAGES: usize = 80;
    const TEXT_BYTES: usize = 1024 * 1024;
    let mut harness = Harness::new(vec![]).await?;
    harness.initialize().await?;
    let session = harness.session().await?;
    harness.stop().await?;
    let state = harness.root.path().join("state/data");
    let persistence = Persistence::open(&state).await?;
    let mut parent_id = None;
    for index in 0..MESSAGES {
        let text = format!("{index:03}:{}", "x".repeat(TEXT_BYTES));
        let content = if index % 2 == 0 {
            ConversationItem::User {
                content: text.into(),
            }
        } else {
            ConversationItem::Assistant {
                items: vec![AssistantItem::Text {
                    phase: AssistantPhase::FinalAnswer,
                    text,
                }],
            }
        };
        let message = Message {
            id: MessageId::new(format!("replay-{index}")),
            parent_id,
            content,
            status: MessageStatus::Complete,
            agent_profile_id: None,
            generation: None,
        };
        persistence.messages().save(&message).await?;
        parent_id = Some(message.id);
    }
    let mut metadata = persistence
        .sessions()
        .get(&session)
        .await?
        .ok_or_else(|| eyre!("missing persisted session"))?;
    metadata.tip_id = parent_id;
    persistence.sessions().save(&metadata).await?;
    harness.restart().await?;
    harness.initialize().await?;
    harness.send(json!({"jsonrpc":"2.0","id":2,"method":"session/load","params":{"sessionId":session,"cwd":harness.root.path(),"mcpServers":[]}})).await?;
    let mut received = 0;
    loop {
        let value = harness.read().await?;
        if value.get("id") == Some(&json!(2)) {
            assert!(value.get("result").is_some(), "{value}");
            break;
        }
        if let Some(text) = value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
        {
            assert!(text.starts_with(&format!("{received:03}:")));
            assert_eq!(text.len(), TEXT_BYTES + 4);
            assert!(text.bytes().skip(4).all(|byte| byte == b'x'));
            assert_eq!(
                value.pointer("/params/update/messageId"),
                Some(&json!(format!("replay-{received}")))
            );
            received += 1;
        }
    }
    assert_eq!(received, MESSAGES);
    harness.session().await?;
    harness.stop().await
}
