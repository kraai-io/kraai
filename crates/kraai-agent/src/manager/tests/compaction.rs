use super::super::*;
use super::common::{cleanup_dir, test_manager};
use color_eyre::eyre::{Result, eyre};
use kraai_persistence::{CompactionCheckpoint, FileCompactionStore};

fn prompt(prefix: &str, suffix: &str) -> prompts::TurnSystemPrompt {
    prompts::TurnSystemPrompt {
        prefix: prefix.to_string(),
        suffix: suffix.to_string(),
        context_notifications: Vec::new(),
    }
}

fn checkpoint(
    boundary: MessageId,
    previous: Option<MessageId>,
    summary: &str,
) -> CompactionCheckpoint {
    CompactionCheckpoint {
        covered_through: boundary,
        superseded_usage: Vec::new(),
        previous_boundary: previous,
        summary: summary.to_string(),
        model_id: ModelId::new("mock-model"),
        provider_id: ProviderId::new("mock"),
        prompt_version: 1,
        usage: None,
    }
}

async fn context(manager: &AgentManager, session: &str) -> Result<Vec<Message>> {
    let tip = manager
        .get_tip(session)
        .await?
        .ok_or_else(|| eyre!("missing tip"))?;
    manager.get_model_history(&tip).await
}

#[tokio::test]
async fn compaction_restart_selects_latest_ancestor_and_refreshes_system_context() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let first = manager
        .add_message(&session, ChatRole::User, "old request".into(), None)
        .await?;
    let second = manager
        .add_message(&session, ChatRole::Assistant, "old answer".into(), None)
        .await?;
    manager
        .add_message(&session, ChatRole::User, "latest request".into(), None)
        .await?;
    let original = manager.get_chat_history(&session).await?;
    let store = FileCompactionStore::new(&data_dir);
    store
        .save(&checkpoint(first.clone(), None, "obsolete summary"))
        .await?;
    store
        .save(&checkpoint(second, Some(first), "selected summary"))
        .await?;
    let providers = manager.cloned_provider_manager();
    drop(manager);
    let (messages, sessions, _, context_state) = kraai_persistence::init_at(&data_dir).await?;
    let reopened = AgentManager::new(
        providers,
        data_dir.clone(),
        messages,
        sessions,
        context_state,
        Arc::new(kraai_persistence::FileRequestUsageStore::new(&data_dir)),
        data_dir.clone(),
    );
    for (prefix, suffix) in [
        ("current instructions", "file version one"),
        ("new instructions", "file version two"),
    ] {
        let (request, pending) = reopened
            .build_model_context(
                &session,
                context(&reopened, &session).await?,
                &prompt(prefix, suffix),
                None,
                None,
            )
            .await?;
        assert!(pending.is_none());
        assert_eq!(request.messages.len(), 4);
        assert_eq!(
            request.messages.first(),
            Some(&ConversationItem::System {
                text: prefix.into()
            })
        );
        assert_eq!(
            request.messages.last(),
            Some(&ConversationItem::System {
                text: suffix.into()
            })
        );
        let summary = request
            .messages
            .get(1)
            .map(ConversationItem::display_text)
            .ok_or_else(|| eyre!("missing summary"))?;
        assert!(summary.contains("selected summary"));
        assert!(!summary.contains("obsolete summary"));
        assert_eq!(
            request.messages.get(2),
            Some(&ConversationItem::User {
                text: "latest request".into()
            })
        );
    }
    assert_eq!(
        serde_json::to_value(reopened.get_chat_history(&session).await?)?,
        serde_json::to_value(original)?
    );
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn compaction_undo_past_boundary_excludes_abandoned_branch_checkpoint() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    manager
        .add_message(&session, ChatRole::User, "first request".into(), None)
        .await?;
    let shared = manager
        .add_message(&session, ChatRole::Assistant, "shared answer".into(), None)
        .await?;
    manager
        .add_message(&session, ChatRole::User, "abandoned request".into(), None)
        .await?;
    let abandoned = manager
        .add_message(
            &session,
            ChatRole::Assistant,
            "abandoned answer".into(),
            None,
        )
        .await?;
    let store = FileCompactionStore::new(&data_dir);
    store
        .save(&checkpoint(shared.clone(), None, "shared summary"))
        .await?;
    store
        .save(&checkpoint(
            abandoned.clone(),
            Some(shared),
            "abandoned summary",
        ))
        .await?;
    assert_eq!(
        manager.undo_last_user_message(&session).await?,
        Some("abandoned request".into())
    );
    manager
        .add_message(&session, ChatRole::User, "replacement request".into(), None)
        .await?;
    let (request, _) = manager
        .build_model_context(
            &session,
            context(&manager, &session).await?,
            &prompt("system", ""),
            None,
            None,
        )
        .await?;
    let serialized = serde_json::to_string(&request.messages)?;
    assert!(serialized.contains("shared summary"));
    assert!(serialized.contains("replacement request"));
    assert!(!serialized.contains("abandoned"));
    assert!(store.get(&abandoned).await?.is_some());
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn compaction_keeps_latest_covered_user_verbatim_across_repeated_checkpoints() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let user = "Preserve this exact constraint: do not publish.";
    manager
        .add_message(&session, ChatRole::User, user.into(), None)
        .await?;
    let first = manager
        .add_message(&session, ChatRole::Assistant, "first work".into(), None)
        .await?;
    let store = FileCompactionStore::new(&data_dir);
    store
        .save(&checkpoint(first.clone(), None, "first summary"))
        .await?;
    for iteration in 0..2 {
        let (request, _) = manager
            .build_model_context(
                &session,
                context(&manager, &session).await?,
                &prompt("system", "fresh files"),
                None,
                None,
            )
            .await?;
        assert_eq!(
            request
                .messages
                .iter()
                .filter(|item| matches!(item, ConversationItem::User { text } if text == user))
                .count(),
            1
        );
        assert_eq!(
            request.messages.get(1),
            Some(&ConversationItem::User { text: user.into() })
        );
        let summary = request
            .messages
            .get(2)
            .ok_or_else(|| eyre!("missing summary"))?;
        assert!(matches!(summary, ConversationItem::Assistant { .. }));
        assert!(summary.display_text().contains(if iteration == 0 {
            "first summary"
        } else {
            "second summary"
        }));
        if iteration == 0 {
            let second = manager
                .add_message(&session, ChatRole::Assistant, "second work".into(), None)
                .await?;
            store
                .save(&checkpoint(second, Some(first.clone()), "second summary"))
                .await?;
        }
    }
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn compaction_triggers_at_eighty_percent_of_reported_context_usage() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    manager
        .add_message(&session, ChatRole::User, "large input ".repeat(20000), None)
        .await?;
    let assistant = manager
        .add_message(&session, ChatRole::Assistant, "answer".into(), None)
        .await?;
    manager
        .add_message(&session, ChatRole::User, "next".into(), None)
        .await?;
    let system = prompt("instructions", &"pinned contents ".repeat(10000));
    let tool = ScriptToolDefinition {
        name: "tool".into(),
        description: "tool instructions".into(),
    };
    for (input, expected) in [(6799, false), (6800, true), (8800, true), (3200, false)] {
        let mut message = manager.message_store.get(&assistant).await?.unwrap();
        message.generation = Some(MessageGeneration {
            provider_id: ProviderId::new("mock"),
            model_id: ModelId::new("mock-model"),
            max_context: Some(10_000),
            usage: Some(TokenUsage {
                input_tokens: input,
                cache_read_tokens: 700,
                cache_write_tokens: 100,
                output_tokens: 300,
                reasoning_tokens: 100,
                ..Default::default()
            }),
        });
        manager.message_store.save(&message).await?;
        let displayed = manager.get_session_context_usage(&session).await?.unwrap();
        assert_eq!(displayed.usage.used_context_tokens(), input + 1200);
        let (_, pending) = manager
            .build_model_context(
                &session,
                context(&manager, &session).await?,
                &system,
                Some(tool.clone()),
                Some(10_000),
            )
            .await?;
        assert_eq!(pending.is_some(), expected);
    }
    for limit in [None, Some(0), Some(usize::MAX)] {
        let (_, pending) = manager
            .build_model_context(
                &session,
                context(&manager, &session).await?,
                &system,
                None,
                limit,
            )
            .await?;
        assert!(pending.is_none());
    }
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn compaction_does_not_guess_usage_or_reuse_usage_before_a_checkpoint() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    let session = manager.create_session().await?;
    let boundary = manager
        .add_message(&session, ChatRole::User, "large input ".repeat(20000), None)
        .await?;
    let assistant = manager
        .add_message(&session, ChatRole::Assistant, "answer".into(), None)
        .await?;
    let system = prompt("instructions", "pinned contents");
    let (_, pending) = manager
        .build_model_context(
            &session,
            context(&manager, &session).await?,
            &system,
            None,
            Some(10_000),
        )
        .await?;
    assert!(pending.is_none());
    let mut message = manager.message_store.get(&assistant).await?.unwrap();
    message.generation = Some(MessageGeneration {
        provider_id: ProviderId::new("mock"),
        model_id: ModelId::new("mock-model"),
        max_context: Some(10_000),
        usage: Some(TokenUsage {
            input_tokens: 8000,
            ..Default::default()
        }),
    });
    manager.message_store.save(&message).await?;
    let store = FileCompactionStore::new(&data_dir);
    let mut saved = checkpoint(boundary, None, "summary");
    manager
        .add_message(&session, ChatRole::User, "abandoned request".into(), None)
        .await?;
    let abandoned = manager
        .add_message(
            &session,
            ChatRole::Assistant,
            "abandoned answer".into(),
            None,
        )
        .await?;
    let mut abandoned_message = manager.message_store.get(&abandoned).await?.unwrap();
    abandoned_message.generation = message.generation.clone();
    manager.message_store.save(&abandoned_message).await?;
    saved.superseded_usage = vec![assistant, abandoned];
    store.save(&saved).await?;
    assert_eq!(
        manager.undo_last_user_message(&session).await?,
        Some("abandoned request".into())
    );
    manager
        .add_message(&session, ChatRole::User, "continue".into(), None)
        .await?;
    let (_, pending) = manager
        .build_model_context(
            &session,
            context(&manager, &session).await?,
            &system,
            None,
            Some(10_000),
        )
        .await?;
    assert!(pending.is_none());
    let next = manager
        .add_message(&session, ChatRole::Assistant, "new answer".into(), None)
        .await?;
    let mut next_message = manager.message_store.get(&next).await?.unwrap();
    next_message.generation = message.generation;
    manager.message_store.save(&next_message).await?;
    let (_, pending) = manager
        .build_model_context(
            &session,
            context(&manager, &session).await?,
            &system,
            None,
            Some(10_000),
        )
        .await?;
    assert!(pending.is_some());
    cleanup_dir(data_dir).await;
    Ok(())
}
