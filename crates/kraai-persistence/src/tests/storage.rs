use super::*;

#[tokio::test]
async fn message_ids_do_not_load_message_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    connection.execute(
        "INSERT INTO records(kind, id, data) VALUES ('message', 'message', 'invalid JSON'), ('usage', 'usage', 'invalid JSON')",
        [],
    ).unwrap();
    assert!(
        persistence.messages().list_ids().await.unwrap()
            == std::collections::HashSet::from([MessageId::new("message")])
    );
    assert!(
        persistence
            .messages()
            .get(&MessageId::new("message"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn database_creation_records_the_schema_version() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert!(version == database::SCHEMA_VERSION);
    let reopened = open_persistence(directory.path()).await.unwrap();
    assert!(
        reopened
            .sessions()
            .get(&SessionId::new("session"))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn other_schema_versions_are_rejected_without_changing_the_database() {
    let current = database::SCHEMA_VERSION;
    for other in [current - 1, current + 1] {
        let directory = tempfile::tempdir().unwrap();
        let connection =
            rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
        connection
            .pragma_update(None, "user_version", other)
            .unwrap();
        assert!(open_persistence(directory.path()).await.is_err());
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert!(version == other);
        let tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(tables == 0);
    }
}

#[tokio::test]
async fn deletion_preserves_branched_history_and_removes_unlinked_messages() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("original"))
        .await
        .unwrap();
    let mut original_ids = Vec::new();
    for _ in 0..16 {
        let appended = persistence
            .conversations()
            .append_message(request("original", "shared"))
            .await
            .unwrap();
        original_ids.push(appended.message.id);
    }
    let mut fork = session("fork");
    fork.tip_id = original_ids.last().cloned();
    persistence.sessions().save(&fork).await.unwrap();
    let mut other_fork = session("other-fork");
    other_fork.tip_id = original_ids.first().cloned();
    persistence.sessions().save(&other_fork).await.unwrap();
    for _ in 0..16 {
        persistence
            .conversations()
            .append_message(request("original", "exclusive"))
            .await
            .unwrap();
    }
    let mut unlinked = persistence
        .conversations()
        .append_message(request("fork", "fork message"))
        .await
        .unwrap()
        .message;
    let fork_message_id = unlinked.id.clone();
    unlinked.id = MessageId::new("unlinked-original-message");
    unlinked.parent_id = original_ids.last().cloned();
    persistence.messages().save(&unlinked).await.unwrap();
    persistence
        .sessions()
        .delete(&SessionId::new("original"))
        .await
        .unwrap();
    let retained = persistence.messages().list_ids().await.unwrap();
    assert!(retained.len() == original_ids.len() + 1);
    assert!(retained.contains(&fork_message_id));
    for id in &original_ids {
        assert!(retained.contains(id));
    }
    assert!(!retained.contains(&unlinked.id));
    persistence
        .sessions()
        .delete(&SessionId::new("fork"))
        .await
        .unwrap();
    assert!(
        persistence.messages().list_ids().await.unwrap()
            == original_ids.first().cloned().into_iter().collect()
    );
    persistence
        .sessions()
        .delete(&SessionId::new("other-fork"))
        .await
        .unwrap();
    assert!(persistence.messages().list_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn deletion_cannot_remove_another_clients_owned_unlinked_message() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open_persistence(directory.path()).await.unwrap();
    owner.sessions().save(&session("owner")).await.unwrap();
    let root = owner
        .conversations()
        .append_message(request("owner", "root"))
        .await
        .unwrap();
    let mut fork = session("fork");
    fork.tip_id = Some(root.message.id.clone());
    owner.sessions().save(&fork).await.unwrap();
    let mut metadata = owner
        .sessions()
        .get(&SessionId::new("owner"))
        .await
        .unwrap()
        .expect("Missing session");
    metadata.tip_id = None;
    owner.sessions().save(&metadata).await.unwrap();
    owner
        .sessions()
        .claim_turn(&SessionId::new("owner"))
        .await
        .unwrap();
    let observer = open_persistence(directory.path()).await.unwrap();
    assert!(
        observer
            .sessions()
            .delete(&SessionId::new("fork"))
            .await
            .is_err()
    );
    assert!(
        observer
            .sessions()
            .get(&SessionId::new("fork"))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        observer
            .messages()
            .get(&root.message.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn last_used_model_survives_reopening_and_messages_without_generation() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    for (model, effort) in [("first", "light"), ("last", "deep")] {
        let mut request = request("session", "answer");
        request.content = ConversationItem::Assistant { items: Vec::new() };
        request.generation = Some(kraai_types::MessageGeneration {
            provider_id: kraai_types::ProviderId::new("provider"),
            model_id: kraai_types::ModelId::new(model),
            options: kraai_types::ModelOptionValues::from([(
                "reasoning_effort".into(),
                kraai_types::ModelOptionValue::Choice(effort.into()),
            )]),
            max_context: None,
            usage: None,
        });
        persistence
            .conversations()
            .append_message(request)
            .await
            .unwrap();
    }
    persistence
        .conversations()
        .append_message(request("session", "next question"))
        .await
        .unwrap();
    let reopened = open_persistence(directory.path()).await.unwrap();
    let saved = reopened
        .sessions()
        .get(&SessionId::new("session"))
        .await
        .unwrap()
        .expect("Missing session");
    assert!(
        saved.selected_model
            == Some(kraai_types::ModelSelection {
                provider_id: kraai_types::ProviderId::new("provider"),
                model_id: kraai_types::ModelId::new("last"),
                options: kraai_types::ModelOptionValues::from([(
                    "reasoning_effort".into(),
                    kraai_types::ModelOptionValue::Choice("deep".into()),
                )]),
            })
    );
}

#[tokio::test]
async fn repeated_message_reads_observe_changes_from_another_connection() {
    let directory = tempfile::tempdir().unwrap();
    let reader = open_persistence(directory.path()).await.unwrap();
    let writer = open_persistence(directory.path()).await.unwrap();
    let mut message = kraai_types::Message {
        id: MessageId::new("message"),
        parent_id: None,
        content: ConversationItem::User {
            content: "original".into(),
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    writer.messages().save(&message).await.unwrap();
    let original = reader
        .messages()
        .get(&message.id)
        .await
        .unwrap()
        .expect("Missing original message");
    assert!(
        reader
            .messages()
            .get(&MessageId::new("missing"))
            .await
            .unwrap()
            .is_none()
    );
    message.content = ConversationItem::User {
        content: "updated\n\"hello\" 🦀".into(),
    };
    writer.messages().save(&message).await.unwrap();
    let updated = reader
        .messages()
        .get(&message.id)
        .await
        .unwrap()
        .expect("Missing updated message");
    assert!(updated.content == message.content);
    assert!(original.content.text() == Some("original"));
    writer.messages().delete(&message.id).await.unwrap();
    assert!(reader.messages().get(&message.id).await.unwrap().is_none());
    writer.messages().save(&message).await.unwrap();
    assert!(
        reader
            .messages()
            .get(&message.id)
            .await
            .unwrap()
            .is_some_and(|stored| stored.content == message.content)
    );
}

#[tokio::test]
async fn message_read_errors_preserve_types_and_allow_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    let message = kraai_types::Message {
        id: MessageId::new("message"),
        parent_id: None,
        content: ConversationItem::User {
            content: "valid".into(),
        },
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    };
    persistence.messages().save(&message).await.unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    let valid = serde_json::to_string(&message).unwrap();
    for corrupt in ["'invalid JSON'", "'{}'", "X'00'", "CAST(X'FF' AS TEXT)"] {
        connection
            .execute(
                &format!(
                    "UPDATE records SET data = {corrupt} WHERE kind = 'message' AND id = 'message'"
                ),
                [],
            )
            .unwrap();
        let actual = persistence
            .messages()
            .get(&message.id)
            .await
            .expect_err("Corrupt message was accepted");
        match connection.query_row(
            "SELECT data FROM records WHERE kind = 'message' AND id = 'message'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(json) => {
                let expected = serde_json::from_str::<kraai_types::Message>(&json)
                    .expect_err("Expected invalid message JSON");
                assert!(
                    actual
                        .downcast_ref::<serde_json::Error>()
                        .is_some_and(|actual| actual.to_string() == expected.to_string())
                );
            }
            Err(expected) => {
                assert!(actual.downcast_ref::<rusqlite::Error>() == Some(&expected));
            }
        }
        connection
            .execute(
                "UPDATE records SET data = ?1 WHERE kind = 'message' AND id = 'message'",
                [&valid],
            )
            .unwrap();
        assert!(
            persistence
                .messages()
                .get(&message.id)
                .await
                .unwrap()
                .is_some_and(|stored| stored.content == message.content)
        );
    }
}

#[tokio::test]
async fn history_cursor_pages_double_and_detect_cycles_across_pages() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    persistence
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    let conversation = persistence.conversations();
    let mut ids = Vec::new();
    for index in 0..7 {
        ids.push(
            conversation
                .append_message(request("session", &format!("message {index}")))
                .await
                .unwrap()
                .message
                .id,
        );
    }
    ids.reverse();
    let tip = ids.first().cloned();
    let mut cursor = HistoryCursor::new(persistence.messages().as_ref(), tip.clone());
    let mut pages = Vec::new();
    while let Some(page) = cursor.next_page().await.unwrap() {
        pages.push(
            page.into_iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
        );
    }
    assert!(pages.iter().map(Vec::len).collect::<Vec<_>>() == [1, 2, 4]);
    assert!(pages.concat() == ids);

    let root = ids.last().expect("missing root");
    let mut message = persistence
        .messages()
        .get(root)
        .await
        .unwrap()
        .expect("missing root message");
    message.parent_id.clone_from(&tip);
    persistence.messages().save(&message).await.unwrap();
    let mut cursor = HistoryCursor::new(persistence.messages().as_ref(), tip);
    let error = loop {
        match cursor.next_page().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("cycle was not detected"),
            Err(error) => break error,
        }
    };
    assert!(error.to_string().contains("cycle repeats message"));
}

#[tokio::test]
async fn stream_deltas_replay_idempotently_until_a_full_save() {
    use kraai_types::{AssistantItem, AssistantPhase, StreamId};
    let directory = tempfile::tempdir().unwrap();
    let persistence = open_persistence(directory.path()).await.unwrap();
    persistence
        .sessions()
        .save(&session("session"))
        .await
        .unwrap();
    persistence
        .sessions()
        .claim_turn(&SessionId::new("session"))
        .await
        .unwrap();
    let mut streaming = request("session", "unused");
    streaming.content = ConversationItem::Assistant { items: Vec::new() };
    streaming.status = MessageStatus::Streaming {
        stream_id: StreamId::new("stream"),
    };
    let mut message = persistence
        .conversations()
        .append_message(streaming)
        .await
        .unwrap()
        .message;
    let deltas = [
        StreamDelta::Text {
            index: 0,
            phase: AssistantPhase::FinalAnswer,
            text: "hello".into(),
        },
        StreamDelta::Text {
            index: 0,
            phase: AssistantPhase::FinalAnswer,
            text: " world".into(),
        },
    ];
    for delta in &deltas {
        delta.apply(&mut message).unwrap();
    }
    let messages = persistence.messages();
    messages
        .append_stream_deltas(&message, 0, &deltas)
        .await
        .unwrap();
    messages
        .append_stream_deltas(&message, 0, &deltas)
        .await
        .unwrap();
    let reopened = open_persistence(directory.path()).await.unwrap();
    let replayed = reopened
        .messages()
        .get(&message.id)
        .await
        .unwrap()
        .expect("missing streamed message");
    assert!(replayed.content == message.content && replayed.status == message.status);

    message.content = ConversationItem::Assistant {
        items: vec![AssistantItem::Text {
            phase: AssistantPhase::FinalAnswer,
            text: "final".into(),
        }],
    };
    message.status = MessageStatus::Complete;
    messages.save(&message).await.unwrap();
    let saved = messages
        .get(&message.id)
        .await
        .unwrap()
        .expect("missing saved message");
    assert!(saved.content == message.content && saved.status == message.status);
    let connection = rusqlite::Connection::open(directory.path().join("kraai.sqlite3")).unwrap();
    let remaining: i64 = connection
        .query_row("SELECT COUNT(*) FROM stream_deltas", [], |row| row.get(0))
        .unwrap();
    assert!(remaining == 0);
}
