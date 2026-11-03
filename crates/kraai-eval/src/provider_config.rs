use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail, eyre};
use kraai_provider_core::{DynamicValue, ModelConfig, ProviderConfig, ProviderManagerConfig};
use kraai_provider_openai_chat_completions::OpenAiChatCompletionsFactory;

const CODEX_PROXY_TOKEN_ENV: &str = "KRAAI_EVAL_CODEX_PROXY_TOKEN";
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
}

pub(crate) struct PreparedKraaiProviderConfig {
    provider: ProviderConfig,
    models: Vec<ModelConfig>,
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
        }
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
                provider
                    .config
                    .insert(String::from("allow_http_proxy"), DynamicValue::Bool(true));
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
        Ok(PreparedKraaiProviderConfig { provider, models })
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
        let config = self.config_for_url("http://eval-proxy.invalid/backend-api");
        Ok(crate::cache::hash_chunks(&[
            toml::to_string(&config)?.into_bytes(),
            EVAL_AGENT_PROFILES.as_bytes().to_vec(),
        ]))
    }

    pub(crate) fn selected_provider_id(&self) -> &str {
        self.provider.id.as_str()
    }

    pub(crate) fn materialize(&self, workspace: &Path, proxy_url: &str) -> Result<PathBuf> {
        let config = self.config_for_url(proxy_url);
        let directory = workspace.join(".kraai-eval");
        fs::create_dir_all(&directory)?;
        let path = directory.join("providers.toml");
        fs::write(&path, toml::to_string_pretty(&config)?)?;
        fs::write(directory.join("agents.toml"), EVAL_AGENT_PROFILES)?;
        Ok(path)
    }

    fn config_for_url(&self, proxy_url: &str) -> ProviderManagerConfig {
        let mut provider = self.provider.clone();
        provider.config.insert(
            String::from("base_url"),
            DynamicValue::String(proxy_url.to_owned()),
        );
        ProviderManagerConfig {
            providers: vec![provider],
            models: self.models.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;

    #[test]
    fn prepared_config_keeps_identity_and_materialization_on_one_snapshot() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-provider-snapshot-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let source = root.join("providers.toml");
        fs::write(
            &source,
            "[[provider]]\nid = 'original'\ntype = 'openai-codex'\nbase_url = 'https://original.invalid'\n[[model]]\nid = 'model'\nprovider_id = 'original'\nname = ' Original Model '\n",
        )?;
        let request = KraaiProviderConfigRequest::new(source.clone(), None);
        let prepared = request.prepare()?;
        let digest = prepared.digest()?;
        let expected: ProviderManagerConfig = toml::from_str(
            "[[provider]]\nid = 'original'\ntype = 'openai-codex'\nbase_url = 'http://eval-proxy.invalid/backend-api'\nproxy_token_env = 'KRAAI_EVAL_CODEX_PROXY_TOKEN'\nallow_http_proxy = true\n[[model]]\nid = 'model'\nprovider_id = 'original'\nname = ' Original Model '\n",
        )?;
        ensure!(
            digest
                == crate::cache::hash_chunks(&[
                    toml::to_string(&expected)?.into_bytes(),
                    EVAL_AGENT_PROFILES.as_bytes().to_vec(),
                ])
        );

        fs::write(
            &source,
            "[[provider]]\nid = 'replacement'\ntype = 'openai-codex'\n",
        )?;
        let path = prepared.materialize(&root, "http://127.0.0.1:1234/backend-api")?;
        let materialized: ProviderManagerConfig = toml::from_slice(&fs::read(path)?)?;
        ensure!(prepared.selected_provider_id() == "original");
        ensure!(prepared.digest()? == digest);
        ensure!(
            materialized
                .providers
                .first()
                .is_some_and(|provider| provider.id.as_str() == "original")
        );
        ensure!(materialized.models == expected.models);
        ensure!(request.prepare()?.digest()? != digest);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn sanitizer_keeps_only_selected_codex_provider_and_models() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-provider-config-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let source = root.join("providers.toml");
        fs::write(
            &source,
            r#"
[[provider]]
id = "codex-main"
type = "openai-codex"
base_url = "https://should-be-overridden.invalid"

[[provider]]
id = "api-key-provider"
type = "openai-chat-completions"
api_key = "must-not-leak"
base_url = "https://api.openai.com/v1"

[[model]]
id = "gpt-5.6-high"
provider_id = "codex-main"
name = "GPT-5.6 High"

[[model]]
id = "secret-model"
provider_id = "api-key-provider"
"#,
        )?;
        let request =
            KraaiProviderConfigRequest::new(source, Some(String::from("codex-main"))).prepare()?;
        ensure!(request.selected_provider_id() == "codex-main");
        let workspace = root.join("workspace");
        fs::create_dir(&workspace)?;
        let output = request.materialize(&workspace, "http://127.0.0.1:1234/backend-api")?;
        let rendered = fs::read_to_string(output)?;
        ensure!(
            rendered.contains("codex-main"),
            "selected provider was removed"
        );
        ensure!(
            rendered.contains("gpt-5.6-high"),
            "selected model was removed"
        );
        ensure!(
            rendered.contains("KRAAI_EVAL_CODEX_PROXY_TOKEN"),
            "proxy token environment was not configured"
        );
        ensure!(
            rendered.contains("http://127.0.0.1:1234/backend-api"),
            "proxy URL was not configured"
        );
        ensure!(!rendered.contains("must-not-leak"), "API key leaked");
        ensure!(
            !rendered.contains("secret-model"),
            "unselected model leaked"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn sanitizer_routes_chat_completions_provider_through_proxy_token() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-chat-provider-config-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let source = root.join("providers.toml");
        fs::write(
            &source,
            r#"
[[provider]]
id = "codex-main"
type = "openai-codex"

[[provider]]
id = "local"
type = "openai-chat-completions"
api_key = "must-not-leak"
base_url = "http://127.0.0.1:10014/v1"
only_listed_models = false
custom_header = "must-not-leak-either"

[[model]]
id = "local-model"
provider_id = "local"
"#,
        )?;
        ensure!(
            KraaiProviderConfigRequest::new(source.clone(), None)
                .kind()
                .is_err_and(|error| error.to_string().contains("found: codex-main, local"))
        );
        let request = KraaiProviderConfigRequest::new(source, Some(String::from("local")));
        ensure!(request.kind()? == KraaiProviderKind::ChatCompletions);
        let upstream = request.chat_completions_upstream()?;
        ensure!(upstream.provider_id == "local");
        ensure!(upstream.base_url == "http://127.0.0.1:10014/v1");
        ensure!(upstream.api_key == "must-not-leak");

        let path = request
            .prepare()?
            .materialize(&root, "http://127.0.0.1:1234/v1")?;
        let materialized: ProviderManagerConfig = toml::from_slice(&fs::read(path)?)?;
        let expected: ProviderManagerConfig = toml::from_str(
            "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\nbase_url = 'http://127.0.0.1:1234/v1'\ncache_breakpoints = false\nenv_var_api_key = 'OPENAI_API_KEY'\nonly_listed_models = false\nprompt_cache_key = false\n[[model]]\nid = 'local-model'\nprovider_id = 'local'\n",
        )?;
        ensure!(toml::to_string(&materialized)? == toml::to_string(&expected)?);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn chat_completions_cache_hints_survive_the_proxy_base_url() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-chat-cache-hints-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let source = root.join("providers.toml");
        fs::write(
            &source,
            r#"
[[provider]]
id = "openrouter"
type = "openai-chat-completions"
api_key = "key"
base_url = "https://openrouter.ai/api/v1"
prompt_cache_key = true
"#,
        )?;
        let path = KraaiProviderConfigRequest::new(source, None)
            .prepare()?
            .materialize(&root, "http://127.0.0.1:1234/v1")?;
        let materialized: ProviderManagerConfig = toml::from_slice(&fs::read(path)?)?;
        let provider = materialized
            .providers
            .first()
            .ok_or_else(|| eyre!("provider was removed"))?;
        for key in ["cache_breakpoints", "prompt_cache_key"] {
            ensure!(
                provider.config.get(key) == Some(&DynamicValue::Bool(true)),
                "{key} was not kept"
            );
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
