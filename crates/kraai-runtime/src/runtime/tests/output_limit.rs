use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use color_eyre::eyre::{Result, eyre};
use futures::stream::{self, BoxStream};
use kraai_provider_core::{
    Model, ModelConfig, Provider, ProviderManager, ProviderRequest, ProviderRequestContext,
    ProviderStreamEvent, StopReason,
};
use kraai_types::{AssistantPhase, ModelId, ProviderId, ScriptCall, ToolCallId};

use super::super::stream_driver::StreamDriveResult;
use super::harness::{RuntimeTestHarness, create_session_with_profile};

#[derive(Clone, Copy)]
enum CutOff {
    Text,
    /// The provider dropped the cut off call, so the response is empty.
    Nothing,
    /// The provider reported the cut off call as an invalid call.
    Call,
}

/// Stops every response at the output limit until the request was compacted, or until it
/// carries the output limit notice when `obeys_notice` is set.
struct TruncatingProvider {
    model: Model,
    cut_off: CutOff,
    obeys_notice: bool,
    compaction_fails: bool,
    requests: Mutex<Vec<&'static str>>,
    notices: Mutex<Vec<String>>,
}

#[async_trait]
impl Provider for TruncatingProvider {
    fn get_provider_id(&self) -> ProviderId {
        ProviderId::new("mock")
    }

    async fn list_models(&self) -> Vec<Model> {
        vec![self.model.clone()]
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        _model_id: &ModelId,
        request: ProviderRequest,
        _request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        let text = serde_json::to_string(&request.messages)?;
        let kind = if text.contains("CONTEXT CHECKPOINT COMPACTION") {
            "summary"
        } else if text.contains("summary of the work") {
            "compacted"
        } else if text.contains("reached the output limit") {
            "warned"
        } else {
            "original"
        };
        self.requests
            .lock()
            .map_err(|error| eyre!("{error}"))?
            .push(kind);
        if kind == "warned" {
            self.notices
                .lock()
                .map_err(|error| eyre!("{error}"))?
                .push(text);
        }
        if kind == "summary" && self.compaction_fails {
            return Err(eyre!("summary request failed"));
        }
        let delta = |text: &str| {
            Ok(ProviderStreamEvent::TextDelta {
                item_id: String::from("answer"),
                phase: AssistantPhase::FinalAnswer,
                delta: text.into(),
            })
        };
        let events = match kind {
            "summary" => vec![delta("summary of the work")],
            "compacted" => vec![delta("done")],
            "warned" if self.obeys_notice => vec![delta("done")],
            _ => {
                let mut events = match self.cut_off {
                    CutOff::Text => vec![delta("partial")],
                    CutOff::Nothing => Vec::new(),
                    CutOff::Call => vec![Ok(ProviderStreamEvent::ScriptCall(ScriptCall {
                        call_id: ToolCallId::new("cut"),
                        name: String::from("kraai_nushell"),
                        input: String::from("{\"input\":\"ls"),
                        arguments_error: Some(String::from("cut off at the output token limit")),
                        rewrite: None,
                    }))],
                };
                events.push(Ok(ProviderStreamEvent::Finished {
                    stop_reason: StopReason::MaxOutputTokens,
                }));
                events
            }
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

struct Shared(Arc<TruncatingProvider>);

#[async_trait]
impl Provider for Shared {
    fn get_provider_id(&self) -> ProviderId {
        self.0.get_provider_id()
    }

    async fn list_models(&self) -> Vec<Model> {
        self.0.list_models().await
    }

    async fn register_model(&mut self, _model: ModelConfig) -> Result<()> {
        Ok(())
    }

    async fn generate_reply_stream(
        &self,
        model_id: &ModelId,
        request: ProviderRequest,
        request_context: &ProviderRequestContext,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent>>> {
        self.0
            .generate_reply_stream(model_id, request, request_context)
            .await
    }
}

async fn drive(
    max_context: Option<usize>,
    max_output: Option<usize>,
    cut_off: CutOff,
    obeys_notice: bool,
    compaction_fails: bool,
) -> Result<(StreamDriveResult, Vec<&'static str>, Vec<String>)> {
    let provider = Arc::new(TruncatingProvider {
        model: Model {
            max_context,
            max_output,
            ..super::harness::mock_model()
        },
        cut_off,
        obeys_notice,
        compaction_fails,
        requests: Mutex::default(),
        notices: Mutex::default(),
    });
    let mut providers = ProviderManager::new();
    providers.register_provider(ProviderId::new("mock"), Box::new(Shared(provider.clone())));
    let harness = RuntimeTestHarness::new_with_parts(providers)
        .await
        .expect("runtime regression fixture must initialize");
    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    let agent = &harness.runtime.agent_manager;
    let request = agent
        .prepare_start_stream(
            &session_id,
            "hello".into(),
            ModelId::new("mock-model"),
            ProviderId::new("mock"),
            Default::default(),
        )
        .await?;
    let result = super::harness::drive_stream(
        harness.runtime.clone(),
        session_id,
        agent.cloned_provider_manager(),
        request,
    )
    .await;
    harness.shutdown().await;
    let requests = provider
        .requests
        .lock()
        .map_err(|error| eyre!("{error}"))?
        .clone();
    let notices = provider
        .notices
        .lock()
        .map_err(|error| eyre!("{error}"))?
        .clone();
    Ok((result, requests, notices))
}

fn reached_output_limit(result: &StreamDriveResult) -> Option<bool> {
    match result {
        StreamDriveResult::Completed(output) => Some(output.reached_output_limit),
        _ => None,
    }
}

#[tokio::test]
async fn a_response_cut_off_by_the_context_left_compacts_and_retries() -> Result<()> {
    let (result, requests, _) = drive(Some(32_768), None, CutOff::Text, true, false).await?;
    assert_eq!(reached_output_limit(&result), Some(false), "{result:?}");
    assert_eq!(requests, ["original", "summary", "compacted"]);
    Ok(())
}

#[tokio::test]
async fn a_cut_off_response_stands_when_compaction_fails() -> Result<()> {
    let (result, requests, _) = drive(Some(32_768), None, CutOff::Text, true, true).await?;
    assert_eq!(reached_output_limit(&result), Some(true), "{result:?}");
    assert_eq!(requests, ["original", "summary"]);
    Ok(())
}

#[tokio::test]
async fn a_response_cut_off_by_the_model_limit_retries_with_a_notice() -> Result<()> {
    let (result, requests, notices) =
        drive(Some(1_000_000), Some(1_000), CutOff::Nothing, true, false).await?;
    assert_eq!(reached_output_limit(&result), Some(false), "{result:?}");
    assert_eq!(requests, ["original", "warned"]);
    assert!(
        notices
            .iter()
            .all(|notice| notice.contains("output limit of 1000 tokens"))
    );
    Ok(())
}

#[tokio::test]
async fn a_second_cut_off_after_the_notice_ends_the_response() -> Result<()> {
    let (result, requests, _) = drive(None, Some(1_000), CutOff::Nothing, false, false).await?;
    assert_eq!(reached_output_limit(&result), Some(true), "{result:?}");
    assert_eq!(requests, ["original", "warned"]);
    Ok(())
}

#[tokio::test]
async fn a_cut_off_answer_stands_when_compacting_would_not_help() -> Result<()> {
    let (result, requests, _) = drive(None, Some(1_000), CutOff::Text, true, false).await?;
    assert_eq!(reached_output_limit(&result), Some(true), "{result:?}");
    assert_eq!(requests, ["original"]);
    Ok(())
}

#[tokio::test]
async fn a_cut_off_call_reaches_the_model_as_an_invalid_call() -> Result<()> {
    let (result, requests, _) = drive(None, Some(1_000), CutOff::Call, true, false).await?;
    let StreamDriveResult::Completed(output) = &result else {
        return Err(eyre!("expected a completed response, got {result:?}"));
    };
    assert!(output.protocol_error.is_some());
    assert_eq!(requests, ["original"]);
    let (result, requests, _) = drive(Some(32_768), None, CutOff::Call, true, false).await?;
    assert_eq!(reached_output_limit(&result), Some(false), "{result:?}");
    assert_eq!(requests, ["original", "summary", "compacted"]);
    Ok(())
}
