use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use kraai_provider_core::{DynamicValue, ProviderManagerConfig};

const CODEX_PROVIDER_TYPE: &str = "openai-codex";
const PROXY_TOKEN_ENV: &str = "KRAAI_EVAL_CODEX_PROXY_TOKEN";

#[derive(Debug, Clone)]
pub struct KraaiProviderConfigRequest {
    source: PathBuf,
    provider_id: Option<String>,
}

impl KraaiProviderConfigRequest {
    pub fn new(source: PathBuf, provider_id: Option<String>) -> Self {
        Self {
            source,
            provider_id,
        }
    }

    pub(crate) fn digest(&self) -> Result<String> {
        let config = self.selected_config("http://eval-proxy.invalid/backend-api")?;
        Ok(crate::cache::hash_chunks(&[
            toml::to_string(&config)?.into_bytes()
        ]))
    }

    pub(crate) fn materialize(&self, workspace: &Path, proxy_url: &str) -> Result<PathBuf> {
        let config = self.selected_config(proxy_url)?;
        let directory = workspace.join(".kraai-eval");
        fs::create_dir_all(&directory)?;
        let path = directory.join("providers.toml");
        fs::write(&path, toml::to_string_pretty(&config)?)?;
        Ok(path)
    }

    fn selected_config(&self, proxy_url: &str) -> Result<ProviderManagerConfig> {
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
            String::from("base_url"),
            DynamicValue::String(proxy_url.to_string()),
        );
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
        Ok(ProviderManagerConfig {
            providers: vec![provider],
            models,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;

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
        let request = KraaiProviderConfigRequest::new(source, Some(String::from("codex-main")));
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
