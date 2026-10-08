use super::super::*;
use super::common::{cleanup_dir, test_manager};
use futures::stream::BoxStream;
use kraai_provider_core::{Provider, ProviderRequestContext, ProviderStreamEvent};
use kraai_types::{
    ModelOptionBinding, ModelOptionChoice, ModelOptionDefinition, ModelOptionKind,
    ModelOptionValue, ModelSelection,
};

struct ConfigurableProvider;

#[async_trait::async_trait]
impl Provider for ConfigurableProvider {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("configured")
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![Model {
            id: ModelId::new("model"),
            name: "Configurable model".into(),
            max_context: None,
            supports_images: false,
            options: vec![
                ModelOptionDefinition {
                    id: "effort".into(),
                    label: "Reasoning effort".into(),
                    description: None,
                    required: true,
                    active_when: None,
                    binding: Some(ModelOptionBinding::Body {
                        path: "/reasoning/effort".into(),
                    }),
                    kind: ModelOptionKind::Choice {
                        choices: ["low", "high"]
                            .into_iter()
                            .map(|id| ModelOptionChoice {
                                id: id.into(),
                                label: id.into(),
                                patch: Default::default(),
                            })
                            .collect(),
                    },
                },
                ModelOptionDefinition {
                    id: "force_high".into(),
                    label: "Force high effort".into(),
                    description: None,
                    required: false,
                    active_when: None,
                    binding: None,
                    kind: ModelOptionKind::Boolean {
                        enabled: kraai_types::ModelRequestPatch {
                            body: BTreeMap::from([(
                                "reasoning".into(),
                                serde_json::json!({"effort": "high"}),
                            )]),
                            headers: Default::default(),
                        },
                        disabled: Default::default(),
                    },
                },
            ],
        }]
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
        _: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

fn values(effort: &str) -> ModelOptionValues {
    ModelOptionValues::from([("effort".into(), ModelOptionValue::Choice(effort.into()))])
}

fn selection(options: ModelOptionValues) -> ModelSelection {
    ModelSelection {
        provider_id: ProviderId::new("configured"),
        model_id: ModelId::new("model"),
        options,
    }
}

#[tokio::test]
async fn message_append_failure_keeps_the_validated_selection() -> Result<()> {
    for role in ["user", "assistant"] {
        let (mut manager, data_dir) = test_manager().await;
        manager.providers.register_provider(
            ProviderId::new("configured"),
            Box::new(ConfigurableProvider),
        );
        let session = manager.create_session().await?;
        manager
            .set_session_model(&session, selection(values("low")))
            .await?;
        rusqlite::Connection::open(data_dir.join("kraai.sqlite3"))?.execute_batch(&format!(
            "CREATE TRIGGER fail_append BEFORE INSERT ON records
             WHEN NEW.kind = 'message' AND json_extract(NEW.data, '$.content.type') = '{role}'
             BEGIN SELECT RAISE(FAIL, 'injected {role} append failure'); END;",
        ))?;
        let error = manager
            .prepare_start_stream(
                &session,
                "first".into(),
                ModelId::new("model"),
                ProviderId::new("configured"),
                values("high"),
            )
            .await
            .expect_err("message append fails");
        assert!(format!("{error:?}").contains(&format!("injected {role} append failure")));
        assert_eq!(
            manager.get_session_model(&session).await?,
            Some(selection(values("high")))
        );
        assert!(manager.get_chat_history(&session).await?.is_empty());
        assert!(!manager.is_turn_active(&session));
        assert!(!manager.persistence.sessions().owns_turn(&session).await?);
        cleanup_dir(data_dir).await;
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_stream_recovery_keeps_the_persisted_options() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    manager.providers.register_provider(
        ProviderId::new("configured"),
        Box::new(ConfigurableProvider),
    );
    let session = manager.create_session().await?;
    let request = manager
        .prepare_start_stream(
            &session,
            "first".into(),
            ModelId::new("model"),
            ProviderId::new("configured"),
            values("high"),
        )
        .await?;
    manager.streaming_messages.write().await.clear();
    manager.session_states.remove(&session);
    manager
        .recover_interrupted_stream(manager.require_session(&session).await?)
        .await?;
    assert!(manager.get_message(&request.message_id).await?.is_none());
    assert_eq!(
        manager.get_session_model(&session).await?,
        Some(selection(values("high")))
    );
    assert_eq!(manager.get_chat_history(&session).await?.len(), 1);
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn partial_picker_selection_cannot_generate_or_mutate_history() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    manager.providers.register_provider(
        ProviderId::new("configured"),
        Box::new(ConfigurableProvider),
    );
    let session = manager.create_session().await?;
    let partial = selection(Default::default());
    manager.set_session_model(&session, partial.clone()).await?;
    let mut conflicting = values("low");
    conflicting.insert("force_high".into(), ModelOptionValue::Boolean(true));
    for options in [
        Default::default(),
        values("unsupported"),
        conflicting.clone(),
    ] {
        let error = manager
            .prepare_start_stream(
                &session,
                "first".into(),
                ModelId::new("model"),
                ProviderId::new("configured"),
                options,
            )
            .await
            .expect_err("a concrete supported effort is required");
        assert_eq!(
            error
                .downcast_ref::<kraai_types::DomainError>()
                .map(|error| error.kind()),
            Some(kraai_types::DomainErrorKind::InvalidArgument)
        );
        assert!(manager.get_chat_history(&session).await?.is_empty());
        assert!(!manager.is_turn_active(&session));
        assert!(!manager.persistence.sessions().owns_turn(&session).await?);
        assert_eq!(
            manager.get_session_model(&session).await?,
            Some(partial.clone())
        );
    }
    assert!(
        manager
            .set_session_model(&session, selection(values("unsupported")))
            .await
            .is_err()
    );
    assert!(
        manager
            .set_session_model(&session, selection(conflicting))
            .await
            .is_err()
    );
    assert_eq!(manager.get_session_model(&session).await?, Some(partial));
    cleanup_dir(data_dir).await;
    Ok(())
}

#[tokio::test]
async fn continuation_and_script_recovery_keep_the_original_options() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    manager.providers.register_provider(
        ProviderId::new("configured"),
        Box::new(ConfigurableProvider),
    );
    let session = manager.create_session().await?;
    let original = values("low");
    let first = manager
        .prepare_start_stream(
            &session,
            "first".into(),
            ModelId::new("model"),
            ProviderId::new("configured"),
            original.clone(),
        )
        .await?;
    assert_eq!(first.provider_request.options, original);
    manager.complete_message(&first.message_id).await?;
    let continuation = manager
        .prepare_continuation_stream(&session)
        .await?
        .expect("automatic continuation");
    assert_eq!(continuation.provider_request.options, original);
    manager.complete_message(&continuation.message_id).await?;
    manager.clear_active_turn(&session);
    manager
        .set_session_model(&session, selection(values("high")))
        .await?;
    manager.session_states.remove(&session);
    manager
        .prepare_script_recovery(&session, &first.message_id)
        .await?;
    let recovered = manager
        .prepare_continuation_stream(&session)
        .await?
        .expect("recovered continuation");
    assert_eq!(recovered.provider_request.options, original);
    manager.complete_message(&recovered.message_id).await?;
    let generation = manager
        .get_message(&recovered.message_id)
        .await?
        .expect("persisted message")
        .generation
        .expect("generation metadata");
    assert_eq!(generation.options, original);
    assert_eq!(
        manager.get_session_model(&session).await?,
        Some(selection(original))
    );
    cleanup_dir(data_dir).await;
    Ok(())
}
