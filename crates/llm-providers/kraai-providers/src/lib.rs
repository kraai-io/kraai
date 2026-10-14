#![forbid(unsafe_code)]
//! The provider types Kraai frontends ship with.

use std::path::Path;
use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr, eyre};
use kraai_provider_core::{ProviderRegistry, ProviderSet, ProviderSetup};
use kraai_provider_openai_chat_completions::{OpenAiChatCompletionsFactory, OpenAiFactory};
use kraai_provider_openai_codex::{
    OpenAiCodexAuthController, OpenAiCodexAuthControllerOptions, OpenAiCodexFactory,
};

/// OpenAI-compatible chat completions, OpenAI, and OpenAI Codex with account sign-in.
pub struct DefaultProviders;

#[async_trait::async_trait]
impl ProviderSet for DefaultProviders {
    async fn load(&self, storage_root: &Path) -> Result<ProviderSetup> {
        let options = OpenAiCodexAuthControllerOptions::new(
            storage_root.to_path_buf(),
            storage_root.join("provider-state/openai-codex/auth.json"),
        );
        let codex_auth = Arc::new(
            tokio::task::spawn_blocking(move || {
                OpenAiCodexAuthController::new_with_options(options)
            })
            .await
            .wrap_err("OpenAI auth initialization task failed")?
            .wrap_err("Failed to initialize OpenAI auth")?,
        );
        let mut registry = ProviderRegistry::default();
        registry
            .register_factory::<OpenAiChatCompletionsFactory>()
            .map_err(|error| eyre!(error.to_string()))?;
        registry
            .register_factory::<OpenAiFactory>()
            .map_err(|error| eyre!(error.to_string()))?;
        let codex = OpenAiCodexFactory::new(codex_auth.clone());
        registry
            .register_dynamic_factory(
                OpenAiCodexFactory::TYPE_ID,
                OpenAiCodexFactory::definition(),
                OpenAiCodexFactory::pricing_policy(),
                move |id, config| {
                    codex.create(id, config).map_err(|error| {
                        kraai_provider_core::ProviderError::ConfigParseError(error.to_string())
                    })
                },
                OpenAiCodexFactory::validate_provider_config,
                OpenAiCodexFactory::validate_model_config,
            )
            .map_err(|error| eyre!(error.to_string()))?;
        Ok(ProviderSetup {
            registry,
            auth: vec![codex_auth],
        })
    }
}
