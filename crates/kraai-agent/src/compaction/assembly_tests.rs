use super::*;

#[test]
fn assembly_preserves_history_and_file_snapshot_order() {
    let user = ConversationItem::User {
        content: "request".into(),
    };
    let reply = ConversationItem::Assistant {
        items: vec![AssistantItem::Text {
            phase: AssistantPhase::FinalAnswer,
            text: "reply".into(),
        }],
    };
    let replacement = ConversationItem::Assistant {
        items: vec![AssistantItem::Text {
            phase: AssistantPhase::FinalAnswer,
            text: "earlier summary".into(),
        }],
    };
    let previous = CompactionCheckpoint {
        covered_through: MessageId::new("old"),
        superseded_usage: Vec::new(),
        previous_boundary: None,
        replacement: vec![replacement.clone()],
        provider_id: ProviderId::new("test"),
        model_id: ModelId::new("model"),
        prompt_version: 1,
        usage: None,
    };
    let snapshots: Vec<_> = [
        ("reply", "reply file"),
        ("old", "first unanchored file"),
        ("user", "first user file"),
        ("missing", "second unanchored file"),
        ("user", "second user file"),
    ]
    .into_iter()
    .map(|(anchor, text)| FileContextSnapshot {
        path: text.into(),
        opened_event: "opened".into(),
        anchor: MessageId::new(anchor),
        text: text.into(),
    })
    .collect();
    let file = |text: &str| ConversationItem::FileContext { text: text.into() };
    let request = assemble(
        "instructions",
        &snapshots,
        Some(&previous),
        vec![
            message("user", user.clone()),
            message("reply", reply.clone()),
        ],
        None,
        &Default::default(),
    );
    assert_eq!(
        request.messages,
        vec![
            ConversationItem::System {
                text: "instructions".into(),
            },
            replacement,
            file("first unanchored file"),
            file("second unanchored file"),
            user,
            file("first user file"),
            file("second user file"),
            reply,
            file("reply file"),
        ],
    );
}

#[tokio::test]
async fn missing_boundary_fails_before_requesting_compaction() -> Result<()> {
    let (mut context, providers, requests, root) = fixture(false, Vec::new(), false);
    context.covered_through = None;
    let error = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await
        .err()
        .ok_or_else(|| eyre!("missing boundary should fail"))?;
    ensure!(error.to_string() == "Missing compaction boundary");
    ensure!(
        requests
            .lock()
            .map_err(|error| eyre!("{error}"))?
            .is_empty()
    );
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}

#[tokio::test]
async fn compaction_preserves_current_and_previous_boundaries() -> Result<()> {
    let (mut context, providers, _, root) = fixture(
        false,
        vec![ProviderStreamEvent::TextDelta {
            item_id: "summary".into(),
            phase: AssistantPhase::FinalAnswer,
            delta: "Continue the task".into(),
        }],
        false,
    );
    let previous = MessageId::new("previous");
    context.previous_boundary = Some(previous.clone());
    context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await?;
    let saved = context
        .store
        .get(&MessageId::new("result"))
        .await?
        .ok_or_else(|| eyre!("missing compaction checkpoint"))?;
    ensure!(saved.covered_through == MessageId::new("result"));
    ensure!(saved.previous_boundary.as_ref() == Some(&previous));
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}
