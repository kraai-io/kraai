use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use kraai_provider_core::{DynamicValue, ModelConfig, ProviderConfig, ProviderManagerConfig};

const CODEX_PROVIDER_TYPE: &str = "openai-codex";
const PROXY_TOKEN_ENV: &str = "KRAAI_EVAL_CODEX_PROXY_TOKEN";
const EVAL_AGENT_PROFILES: &str = include_str!("eval-agents.toml");

#[derive(Debug, Clone)]
pub struct KraaiProviderConfigRequest {
    source: PathBuf,
    provider_id: Option<String>,
}

pub(crate) struct PreparedKraaiProviderConfig {
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

    pub(crate) fn prepare(&self) -> Result<PreparedKraaiProviderConfig> {
        let bytes = fs::read(&self.source)
            .wrap_err_with(|| format!("read provider config {}", self.source.display()))?;
        let config: ProviderManagerConfig = toml::from_slice(&bytes)
            .wrap_err_with(|| format!("parse provider config {}", self.source.display()))?;
        let mut matching = config
            .providers
            .into_iter()
            .filter(|provider| provider.type_id == CODEX_PROVIDER_TYPE)
            .filter(|provider| {
                self.provider_id
                    .as_deref()
                    .is_none_or(|id| provider.id.as_str() == id)
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            bail!(
                "expected exactly one matching openai-codex provider in {}, found {}",
                self.source.display(),
                matching.len()
            );
        }
        let mut provider = matching.remove(0);
        provider.config.clear();
        provider.config.insert(
            String::from("proxy_token_env"),
            DynamicValue::String(String::from(PROXY_TOKEN_ENV)),
        );
        let provider_id = provider.id.clone();
        let models = config
            .models
            .into_iter()
            .filter(|model| model.provider_id == provider_id)
            .collect();
        Ok(PreparedKraaiProviderConfig { provider, models })
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
            "[[provider]]\nid = 'original'\ntype = 'openai-codex'\nbase_url = 'http://eval-proxy.invalid/backend-api'\nproxy_token_env = 'KRAAI_EVAL_CODEX_PROXY_TOKEN'\n[[model]]\nid = 'model'\nprovider_id = 'original'\nname = ' Original Model '\n",
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
}
