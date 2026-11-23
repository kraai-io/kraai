use super::*;

#[test]
fn assembly_preserves_history_and_context_section_order() {
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
        &RefreshedContextState {
            snapshots,
            instructions: [
                (None, "top instructions"),
                (Some("old"), "compacted instructions"),
                (Some("user"), "changed instructions"),
            ]
            .into_iter()
            .map(|(anchor, text)| kraai_persistence::InstructionSnapshot {
                key: text.into(),
                label: text.into(),
                anchor: anchor.map(MessageId::new),
                text: text.into(),
            })
            .collect(),
            ..Default::default()
        },
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
            file("top instructions"),
            file("compacted instructions"),
            file("first unanchored file"),
            file("second unanchored file"),
            user,
            file("changed instructions"),
            file("first user file"),
            file("second user file"),
            reply,
            file("reply file"),
        ],
    );
}

#[tokio::test]
async fn missing_boundary_fails_before_requesting_compaction() {
    let (mut context, providers, requests, root) = fixture(false, Vec::new(), false);
    context.covered_through = None;
    let error = context
        .run(&providers, &ProviderId::new("test"), &ModelId::new("model"))
        .await
        .err()
        .expect("missing boundary should fail");
    assert!(error.to_string() == "Missing compaction boundary");
    assert!(requests.lock().unwrap().is_empty());
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn compaction_preserves_current_and_previous_boundaries() {
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
        .await
        .unwrap();
    let saved = context
        .store
        .get(&MessageId::new("result"))
        .await
        .unwrap()
        .expect("missing compaction checkpoint");
    assert!(saved.covered_through == MessageId::new("result"));
    assert!(saved.previous_boundary.as_ref() == Some(&previous));
    tokio::fs::remove_dir_all(root).await.unwrap();
}
