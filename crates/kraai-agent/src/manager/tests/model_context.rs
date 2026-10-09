use super::super::*;
use super::common::{cleanup_dir, test_manager};
use futures::stream::BoxStream;
use kraai_provider_core::{Provider, ProviderRequestContext, ProviderStreamEvent};
use kraai_types::ModelSelection;

struct ContextProvider {
    id: ProviderId,
    standard_limit: usize,
}

#[async_trait::async_trait]
impl Provider for ContextProvider {
    fn get_provider_id(&self) -> ProviderId {
        self.id.clone()
    }

    async fn list_models(&self) -> Vec<Model> {
        [
            ("standard", Some(self.standard_limit)),
            ("large", Some(20_000)),
            ("small", Some(5_000)),
            ("unknown", None),
        ]
        .into_iter()
        .map(|(id, max_context)| Model {
            id: ModelId::new(id),
            name: id.into(),
            max_context,
            supports_images: false,
            options: Vec::new(),
        })
        .collect()
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

#[tokio::test]
async fn model_switches_use_current_context_limits_after_restarting() -> Result<()> {
    let (mut manager, data_dir) = test_manager().await;
    for (id, standard_limit) in [("first", 10_000), ("second", 30_000)] {
        manager.providers.register_provider(
            ProviderId::new(id),
            Box::new(ContextProvider {
                id: ProviderId::new(id),
                standard_limit,
            }),
        );
    }
    let session = manager.create_session().await?;
    let mut previous_usage = None;
    for (provider, model, max_context, compact) in [
        ("first", "standard", Some(10_000), false),
        ("first", "large", Some(20_000), false),
        ("first", "small", Some(5_000), true),
        ("first", "standard", Some(10_000), true),
        ("second", "standard", Some(30_000), false),
        ("second", "unknown", None, false),
    ] {
        let selection = ModelSelection {
            provider_id: ProviderId::new(provider),
            model_id: ModelId::new(model),
            options: Default::default(),
        };
        manager
            .set_session_model(&session, selection.clone())
            .await?;
        let providers = manager.cloned_provider_manager();
        drop(manager);
        let persistence = kraai_persistence::Persistence::open(&data_dir).await?;
        manager = AgentManager::new(providers, data_dir.clone(), persistence, data_dir.clone());
        let snapshot = manager
            .capture_session_snapshot(&session)
            .await?
            .load()
            .await?;
        assert_eq!(snapshot.session.selected_model, Some(selection.clone()));
        assert_eq!(snapshot.context_usage, previous_usage);

        let request = manager
            .prepare_start_stream(
                &session,
                "continue".into(),
                selection.model_id.clone(),
                selection.provider_id.clone(),
                selection.options,
            )
            .await?;
        assert_eq!(request.context_compaction.is_some(), compact);
        let generation = manager
            .get_chat_history(&session)
            .await?
            .remove(&request.message_id)
            .expect("prepared message")
            .generation
            .expect("generation metadata");
        assert_eq!(generation.provider_id, selection.provider_id);
        assert_eq!(generation.model_id, selection.model_id);
        assert_eq!(generation.max_context, max_context);
        manager
            .set_streaming_message_usage(
                &request.message_id,
                TokenUsage {
                    input_tokens: 8_000,
                    total_tokens: 8_000,
                    ..Default::default()
                },
            )
            .await?;
        manager.complete_message(&request.message_id).await?;
        manager.clear_active_turn(&session);
        manager
            .persistence
            .sessions()
            .release_turn(&session)
            .await?;
        previous_usage = manager.get_session_context_usage(&session).await?;
        assert_eq!(
            previous_usage
                .as_ref()
                .expect("completed usage")
                .max_context,
            max_context
        );
    }
    cleanup_dir(data_dir).await;
    Ok(())
}
