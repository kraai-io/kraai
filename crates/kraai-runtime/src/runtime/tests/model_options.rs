use std::collections::VecDeque;

use color_eyre::eyre::{Result, eyre};
use futures::stream::BoxStream;
use kraai_provider_core::{Model, Provider, ProviderManager, ProviderRequest, ProviderStreamEvent};
use kraai_types::{
    AssistantPhase, ModelId, ModelOptionBinding, ModelOptionChoice, ModelOptionDefinition,
    ModelOptionKind, ModelOptionValue, ModelOptionValues, ModelSelection, ProviderId,
};
use tokio::sync::{Mutex, mpsc};

use super::harness::{RuntimeTestHarness, TEST_TIMEOUT, create_session_with_profile};
use crate::{RuntimeErrorKind, SubmitMessageOutcome};

type OutputReceiver = mpsc::UnboundedReceiver<Result<ProviderStreamEvent>>;

struct ConfigurableProvider {
    requests: mpsc::UnboundedSender<ProviderRequest>,
    outputs: Mutex<VecDeque<OutputReceiver>>,
}

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
                    id: "cache".into(),
                    label: "Cache".into(),
                    description: None,
                    required: false,
                    active_when: None,
                    binding: Some(ModelOptionBinding::Body {
                        path: "/cache".into(),
                    }),
                    kind: ModelOptionKind::Boolean {
                        enabled: Default::default(),
                        disabled: Default::default(),
                    },
                },
                ModelOptionDefinition {
                    id: "token_limit".into(),
                    label: "Token limit".into(),
                    description: None,
                    required: false,
                    active_when: None,
                    binding: Some(ModelOptionBinding::Body {
                        path: "/max_output_tokens".into(),
                    }),
                    kind: ModelOptionKind::Integer {
                        min: Some(0),
                        max: Some(4096),
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
        request: ProviderRequest,
        _: &kraai_provider_core::ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        self.requests.send(request)?;
        let output = self
            .outputs
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| eyre!("Missing output"))?;
        Ok(Box::pin(futures::stream::unfold(
            output,
            |mut output| async move { output.recv().await.map(|event| (event, output)) },
        )))
    }
}

fn values(effort: &str) -> ModelOptionValues {
    ModelOptionValues::from([("effort".into(), ModelOptionValue::Choice(effort.into()))])
}

#[tokio::test]
async fn full_and_partial_model_options_survive_restarting_each_session() -> Result<()> {
    let data_dir = std::env::temp_dir().join(format!(
        "kraai-runtime-model-options-{}",
        ulid::Ulid::generate()
    ));
    let selections = [
        ModelSelection {
            provider_id: ProviderId::new("configured"),
            model_id: ModelId::new("model"),
            options: ModelOptionValues::from([
                ("effort".into(), ModelOptionValue::Choice("high".into())),
                ("cache".into(), ModelOptionValue::Boolean(false)),
                ("token_limit".into(), ModelOptionValue::Integer(0)),
            ]),
        },
        ModelSelection {
            provider_id: ProviderId::new("configured"),
            model_id: ModelId::new("model"),
            options: ModelOptionValues::from([
                ("cache".into(), ModelOptionValue::Boolean(true)),
                ("token_limit".into(), ModelOptionValue::Integer(256)),
            ]),
        },
    ];
    let mut sessions = Vec::new();
    for restart in [false, true] {
        let (requests, _received) = mpsc::unbounded_channel();
        let mut providers = ProviderManager::new();
        providers.register_provider(
            ProviderId::new("configured"),
            Box::new(ConfigurableProvider {
                requests,
                outputs: Mutex::default(),
            }),
        );
        let harness = RuntimeTestHarness::new_in_directory(providers, None, Some(data_dir.clone()))
            .await
            .expect("runtime fixture");
        if !restart {
            for selection in &selections {
                let session = create_session_with_profile(&harness.handle, "test-profile").await?;
                harness
                    .handle
                    .set_session_model(session.clone(), selection.clone())
                    .await?;
                sessions.push(session);
            }
        }
        for (session, selection) in sessions.iter().zip(&selections) {
            assert!(harness.handle.load_session(session.clone()).await?);
            assert_eq!(
                harness.handle.get_session_model(session.clone()).await?,
                Some(selection.clone())
            );
            let snapshot = harness.handle.get_session_snapshot(session.clone()).await?;
            assert_eq!(snapshot.session.selected_model.as_ref(), Some(selection));
            assert!(snapshot.history.is_empty());
            assert!(!snapshot.session.is_running);
        }
        harness.shutdown().await;
    }
    tokio::fs::remove_dir_all(data_dir).await?;
    Ok(())
}

#[tokio::test]
async fn queue_and_generation_use_validated_submission_options() -> Result<()> {
    let (requests, mut received) = mpsc::unbounded_channel();
    let (first_output, first_receiver) = mpsc::unbounded_channel();
    let (_second_output, second_receiver) = mpsc::unbounded_channel();
    let mut providers = ProviderManager::new();
    providers.register_provider(
        ProviderId::new("configured"),
        Box::new(ConfigurableProvider {
            requests,
            outputs: Mutex::new(VecDeque::from([first_receiver, second_receiver])),
        }),
    );
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime fixture");
    let session = create_session_with_profile(&harness.handle, "test-profile").await?;
    let models = harness.handle.list_models().await?;
    let model = models
        .get("configured")
        .and_then(|models| models.first())
        .expect("model");
    assert_eq!(model.options.len(), 3);
    assert!(model.options.first().expect("option").required);
    let error = harness
        .handle
        .send_message(
            session.clone(),
            "invalid".into(),
            "model".into(),
            "configured".into(),
            Default::default(),
        )
        .await
        .expect_err("reasoning selection is required");
    assert_eq!(error.kind, RuntimeErrorKind::InvalidArgument);
    assert!(
        harness
            .handle
            .get_chat_history(session.clone())
            .await?
            .is_empty()
    );
    assert!(!harness.runtime.session_store.owns_turn(&session).await?);
    harness
        .handle
        .send_message(
            session.clone(),
            "first".into(),
            "model".into(),
            "configured".into(),
            values("low"),
        )
        .await?;
    let first = tokio::time::timeout(TEST_TIMEOUT, received.recv())
        .await?
        .expect("first request");
    assert_eq!(first.options, values("low"));
    let before = harness.handle.get_chat_history(session.clone()).await?;
    let error = harness
        .handle
        .send_message(
            session.clone(),
            "invalid queued".into(),
            "model".into(),
            "configured".into(),
            values("unsupported"),
        )
        .await
        .expect_err("invalid queued settings are rejected");
    assert_eq!(error.kind, RuntimeErrorKind::InvalidArgument);
    assert!(
        harness
            .runtime
            .queued_messages
            .lock()
            .await
            .get(&session)
            .is_none()
    );
    assert_eq!(
        serde_json::to_value(before)?,
        serde_json::to_value(harness.handle.get_chat_history(session.clone()).await?)?
    );
    let mut queued = values("high");
    queued.insert("cache".into(), ModelOptionValue::Boolean(false));
    queued.insert("token_limit".into(), ModelOptionValue::Integer(0));
    assert_eq!(
        harness
            .handle
            .send_message(
                session.clone(),
                "queued".into(),
                "model".into(),
                "configured".into(),
                queued.clone(),
            )
            .await?,
        SubmitMessageOutcome::Queued { position: 1 }
    );
    assert_eq!(
        harness
            .runtime
            .queued_messages
            .lock()
            .await
            .get(&session)
            .and_then(|queue| queue.front())
            .expect("queued snapshot")
            .options,
        queued
    );
    first_output.send(Ok(ProviderStreamEvent::TextDelta {
        item_id: "answer".into(),
        phase: AssistantPhase::FinalAnswer,
        delta: "first answer".into(),
    }))?;
    drop(first_output);
    let second = tokio::time::timeout(TEST_TIMEOUT, received.recv())
        .await?
        .expect("queued request");
    assert_eq!(second.options, queued);
    let snapshot = harness.handle.get_session_snapshot(session.clone()).await?;
    assert_eq!(
        snapshot
            .session
            .selected_model
            .as_ref()
            .expect("selection")
            .options,
        queued
    );
    assert!(snapshot.history.values().any(|message| {
        message
            .generation
            .as_ref()
            .is_some_and(|generation| generation.options == queued)
    }));
    harness.shutdown().await;
    Ok(())
}
