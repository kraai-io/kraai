use std::time::Instant;

use color_eyre::Result;
use futures::StreamExt;
use kraai_agent::AuxiliaryUsageRecorder;
use kraai_provider_core::{
    CacheWarmup, ProviderManager, ProviderRequestContext, ProviderStreamEvent,
};
use kraai_types::{ModelId, ProviderId};

pub(super) async fn warm_cache(
    providers: &ProviderManager,
    provider_id: &ProviderId,
    model_id: &ModelId,
    warmup: CacheWarmup,
    recorder: AuxiliaryUsageRecorder,
    images: std::sync::Arc<dyn kraai_provider_core::ImageResolver>,
) -> Result<()> {
    let session_id = recorder.session_id.clone();
    let observer = recorder
        .start(providers, provider_id, model_id, "cache-warming")
        .await?;
    let request_id = observer.snapshot().await.message_id;
    let started = Instant::now();
    let context = ProviderRequestContext::with_retry_observer_and_prompt_cache_key(
        observer.clone(),
        session_id,
    )
    .with_image_resolver(images);
    let result = tokio::time::timeout(warmup.timeout, async {
        let mut stream = match providers
            .generate_reply_stream(
                provider_id.clone(),
                model_id,
                warmup.request.clone(),
                context,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(request_id = %request_id, elapsed_ms = started.elapsed().as_millis(), error = %format!("{error:#}"), "Cache warming failed to start");
                return Ok(false);
            }
        };
        let mut received_usage = false;
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderStreamEvent::Usage(usage)) => {
                    observer.save_usage(usage).await?;
                    received_usage = true;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(request_id = %request_id, elapsed_ms = started.elapsed().as_millis(), error = %format!("{error:#}"), "Cache warming stream failed");
                    return Ok(false);
                }
            }
            tokio::task::yield_now().await;
        }
        if !received_usage {
            tracing::warn!("Cache warming ended without usage");
        }
        Ok::<_, color_eyre::Report>(received_usage)
    })
    .await;
    match result {
        Ok(result) => {
            if result? && let Some(usage) = observer.snapshot().await.usage {
                warmup.complete(&usage)?;
            }
        }
        Err(error) => {
            tracing::warn!(request_id = %request_id, elapsed_ms = started.elapsed().as_millis(), error = %format!("{error:#}"), "Cache warming timed out; continuing with the conversation")
        }
    }
    Ok(())
}
