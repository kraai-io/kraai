use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, eyre};
use kraai_provider_core::{
    DynamicConfig, DynamicValue, ModelConfig, ProviderConfig, ProviderManagerConfig,
};
use kraai_provider_openai_chat_completions::OpenAiChatCompletionsFactory;
use kraai_types::ModelId;

const CODEX_PROXY_TOKEN_ENV: &str = "KRAAI_EVAL_CODEX_PROXY_TOKEN";
const MAX_CONTEXT_KEY: &str = "max_context";
const OPENAI_PROXY_TOKEN_ENV: &str = "OPENAI_API_KEY";
const EVAL_AGENT_PROFILES: &str = include_str!("eval-agents.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KraaiProviderKind {
    Codex,
    ChatCompletions,
}

impl KraaiProviderKind {
    fn from_type_id(type_id: &str) -> Option<Self> {
        match type_id {
            "openai-codex" => Some(Self::Codex),
            "openai-chat-completions" => Some(Self::ChatCompletions),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct KraaiProviderConfigRequest {
    source: PathBuf,
    provider_id: Option<String>,
    model_id: Option<String>,
}

pub(crate) struct PreparedKraaiProviderConfig {
    provider: ProviderConfig,
    models: Vec<ModelConfig>,
    model_id: Option<String>,
}

pub(crate) struct ChatCompletionsUpstream {
    pub(crate) provider_id: String,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
}

struct SelectedProvider {
    kind: KraaiProviderKind,
    provider: ProviderConfig,
    models: Vec<ModelConfig>,
}

impl KraaiProviderConfigRequest {
    pub fn new(source: PathBuf, provider_id: Option<String>) -> Self {
        Self {
            source,
            provider_id,
            model_id: None,
        }
    }

    pub(crate) fn has_model(&self) -> bool {
        self.model_id.is_some()
    }

    #[must_use]
    pub fn with_model(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    pub(crate) fn kind(&self) -> Result<KraaiProviderKind> {
        Ok(self.select()?.kind)
    }

    pub(crate) fn prepare(&self) -> Result<PreparedKraaiProviderConfig> {
        let SelectedProvider {
            kind,
            mut provider,
            models,
        } = self.select()?;
        let mut config = std::mem::take(&mut provider.config);
        match kind {
            KraaiProviderKind::Codex => {
                provider.config.insert(
                    String::from("proxy_token_env"),
                    DynamicValue::String(String::from(CODEX_PROXY_TOKEN_ENV)),
                );
            }
            KraaiProviderKind::ChatCompletions => {
                // The proxy replaces base_url, which would change the endpoint-based defaults.
                provider
                    .config
                    .extend(OpenAiChatCompletionsFactory::cache_hint_settings(&config)?);
                for key in ["only_listed_models", "catalog_provider"] {
                    if let Some(value) = config.remove(key) {
                        provider.config.insert(String::from(key), value);
                    }
                }
                provider.config.insert(
                    String::from("env_var_api_key"),
                    DynamicValue::String(String::from(OPENAI_PROXY_TOKEN_ENV)),
                );
            }
        }
        Ok(PreparedKraaiProviderConfig {
            provider,
            models,
            model_id: self.model_id.clone(),
        })
    }

    pub(crate) fn chat_completions_upstream(&self) -> Result<ChatCompletionsUpstream> {
        let SelectedProvider { kind, provider, .. } = self.select()?;
        if kind != KraaiProviderKind::ChatCompletions {
            bail!("only chat-completions providers have a configured upstream");
        }
        let setting = |key: &str| {
            provider
                .config
                .get(key)
                .and_then(DynamicValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let base_url = setting("base_url")
            .ok_or_else(|| eyre!("provider {} has no base_url", provider.id.as_str()))?
            .to_owned();
        let api_key = match (setting("api_key"), setting("env_var_api_key")) {
            (Some(key), _) => key.to_owned(),
            (None, Some(name)) => std::env::var(name)
                .ok()
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| eyre!("API key environment variable {name} is unavailable"))?,
            (None, None) => bail!(
                "provider {} has neither api_key nor env_var_api_key",
                provider.id.as_str()
            ),
        };
        Ok(ChatCompletionsUpstream {
            provider_id: provider.id.as_str().to_owned(),
            base_url,
            api_key,
        })
    }

    fn select(&self) -> Result<SelectedProvider> {
        let bytes = fs::read(&self.source)
            .wrap_err_with(|| format!("read provider config {}", self.source.display()))?;
        let config: ProviderManagerConfig = toml::from_slice(&bytes)
            .wrap_err_with(|| format!("parse provider config {}", self.source.display()))?;
        let mut matching = config
            .providers
            .into_iter()
            .filter(|provider| {
                self.provider_id
                    .as_deref()
                    .is_none_or(|id| provider.id.as_str() == id)
            })
            .filter_map(|provider| {
                KraaiProviderKind::from_type_id(&provider.type_id).map(|kind| (kind, provider))
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            let found = matching
                .iter()
                .map(|(_, provider)| provider.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            match &self.provider_id {
                Some(id) => bail!(
                    "{} has no openai-codex or openai-chat-completions provider {id}",
                    self.source.display()
                ),
                None => bail!(
                    "pass --provider to pick one openai-codex or openai-chat-completions provider from {}; found: {found}",
                    self.source.display()
                ),
            }
        }
        let (kind, provider) = matching.remove(0);
        let models = config
            .models
            .into_iter()
            .filter(|model| model.provider_id == provider.id)
            .collect();
        Ok(SelectedProvider {
            kind,
            provider,
            models,
        })
    }
}

impl PreparedKraaiProviderConfig {
    pub(crate) fn digest(&self) -> Result<String> {
        let config = self.config_for_url("http://eval-proxy.invalid/backend-api", None)?;
        Ok(crate::cache::hash_chunks(&[
            toml::to_string(&config)?.into_bytes(),
            EVAL_AGENT_PROFILES.as_bytes().to_vec(),
        ]))
    }

    pub(crate) fn selected_provider_id(&self) -> &str {
        self.provider.id.as_str()
    }

    pub(crate) fn materialize(
        &self,
        workspace: &Path,
        proxy_url: &str,
        max_context: Option<u64>,
    ) -> Result<PathBuf> {
        let config = self.config_for_url(proxy_url, max_context)?;
        let directory = workspace.join(".kraai-eval");
        fs::create_dir_all(&directory)?;
        let path = directory.join("providers.toml");
        fs::write(&path, toml::to_string_pretty(&config)?)?;
        fs::write(directory.join("agents.toml"), EVAL_AGENT_PROFILES)?;
        Ok(path)
    }

    fn config_for_url(
        &self,
        proxy_url: &str,
        max_context: Option<u64>,
    ) -> Result<ProviderManagerConfig> {
        let mut provider = self.provider.clone();
        provider.config.insert(
            String::from("base_url"),
            DynamicValue::String(proxy_url.to_owned()),
        );
        let mut models = self.models.clone();
        if let Some(max_context) = max_context {
            self.cap_context(&mut models, max_context)?;
        }
        Ok(ProviderManagerConfig {
            providers: vec![provider],
            models,
        })
    }

    fn cap_context(&self, models: &mut Vec<ModelConfig>, max_context: u64) -> Result<()> {
        let model_id = self
            .model_id
            .as_deref()
            .ok_or_else(|| eyre!("a task context limit needs the evaluated model"))?;
        if !models.iter().any(|model| model.id.as_str() == model_id) {
            models.push(ModelConfig {
                id: ModelId::new(model_id),
                provider_id: self.provider.id.clone(),
                options: Vec::new(),
                remove_options: Vec::new(),
                config: DynamicConfig::new(),
            });
        }
        let max_context = i64::try_from(max_context)?;
        for model in models
            .iter_mut()
            .filter(|model| model.id.as_str() == model_id)
        {
            let configured = model
                .config
                .get(MAX_CONTEXT_KEY)
                .and_then(DynamicValue::as_integer);
            model.config.insert(
                String::from(MAX_CONTEXT_KEY),
                DynamicValue::Integer(
                    configured.map_or(max_context, |limit| limit.min(max_context)),
                ),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
