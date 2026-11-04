use std::collections::BTreeSet;

use color_eyre::eyre::{Context, Result, bail};
use kraai_provider_openai_codex::{OpenAiCodexAuthController, OpenAiCodexAuthControllerOptions};

use crate::KraaiProviderConfigRequest;

const OPENAI_UPSTREAM: &str = "https://api.openai.com";
const CHATGPT_UPSTREAM: &str = "https://chatgpt.com";

#[derive(Debug, Clone)]
pub(super) enum ProxyCredentialRequest {
    OpenAiApiKey {
        credential_env: String,
    },
    CodexSubscription,
    ChatCompletions {
        provider: KraaiProviderConfigRequest,
    },
}

impl ProxyCredentialRequest {
    pub(super) fn resolve(&self) -> Result<UpstreamCredentials> {
        match self {
            ProxyCredentialRequest::OpenAiApiKey { credential_env } => {
                let credential = std::env::var(credential_env).wrap_err_with(|| {
                    format!(
                        "model proxy credential environment variable {credential_env} is unavailable"
                    )
                })?;
                if credential.trim().is_empty() {
                    bail!("model proxy credential must not be empty");
                }
                Ok(UpstreamCredentials::OpenAiApiKey {
                    credential,
                    credential_env: credential_env.clone(),
                })
            }
            ProxyCredentialRequest::CodexSubscription => codex_credentials(),
            ProxyCredentialRequest::ChatCompletions { provider } => {
                chat_completions_credentials(provider)
            }
        }
    }
}

#[derive(Clone)]
pub(super) enum UpstreamCredentials {
    OpenAiApiKey {
        credential: String,
        credential_env: String,
    },
    Codex {
        controller: OpenAiCodexAuthController,
        account_id: String,
    },
    ChatCompletions {
        credential: String,
        provider_id: String,
        upstream: String,
        base_path: String,
    },
}

impl UpstreamCredentials {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => "openai",
            Self::Codex { .. } => "openai-codex",
            Self::ChatCompletions { .. } => "openai-chat-completions",
        }
    }

    pub(super) fn upstream(&self) -> &str {
        match self {
            Self::OpenAiApiKey { .. } => OPENAI_UPSTREAM,
            Self::Codex { .. } => CHATGPT_UPSTREAM,
            Self::ChatCompletions { upstream, .. } => upstream,
        }
    }

    pub(super) fn base_path(&self) -> &str {
        match self {
            Self::OpenAiApiKey { .. } => "/v1",
            Self::Codex { .. } => "/backend-api",
            Self::ChatCompletions { base_path, .. } => base_path,
        }
    }

    pub(super) fn allowed_paths(&self) -> BTreeSet<String> {
        match self {
            Self::OpenAiApiKey { .. } => openai_allowed_paths(),
            Self::Codex { .. } => codex_allowed_paths(),
            Self::ChatCompletions { base_path, .. } => BTreeSet::from([
                format!("{base_path}/chat/completions"),
                format!("{base_path}/models"),
            ]),
        }
    }

    pub(super) fn credential_source(&self) -> String {
        match self {
            Self::OpenAiApiKey { credential_env, .. } => format!("env:{credential_env}"),
            Self::Codex { account_id, .. } => format!("codex-account:{account_id}"),
            Self::ChatCompletions { provider_id, .. } => format!("kraai-provider:{provider_id}"),
        }
    }

    pub(super) fn fingerprint(&self) -> String {
        let material = match self {
            Self::OpenAiApiKey { credential, .. } | Self::ChatCompletions { credential, .. } => {
                credential.as_bytes()
            }
            Self::Codex { account_id, .. } => account_id.as_bytes(),
        };
        crate::cache::hash_chunks(&[material])
    }
}

fn chat_completions_credentials(
    provider: &KraaiProviderConfigRequest,
) -> Result<UpstreamCredentials> {
    let upstream = provider.chat_completions_upstream()?;
    let mut url = reqwest::Url::parse(&upstream.base_url)
        .wrap_err_with(|| format!("parse base_url of provider {}", upstream.provider_id))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "base_url of provider {} must be an http or https URL without a username, password, query or fragment",
            upstream.provider_id
        );
    }
    let base_path = url.path().trim_end_matches('/').to_owned();
    url.set_path("");
    Ok(UpstreamCredentials::ChatCompletions {
        credential: upstream.api_key,
        provider_id: upstream.provider_id,
        upstream: url.as_str().trim_end_matches('/').to_owned(),
        base_path,
    })
}

fn codex_credentials() -> Result<UpstreamCredentials> {
    let storage_root = kraai_persistence::agent_state_root()?;
    let auth_path = storage_root.join("provider-state/openai-codex/auth.json");
    let controller = OpenAiCodexAuthController::new_with_options(
        OpenAiCodexAuthControllerOptions::new(storage_root, auth_path),
    )?;
    let worker = controller.clone();
    let thread = std::thread::spawn(move || -> Result<String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let auth = runtime.block_on(worker.get_request_auth())?;
        Ok(auth.account_id().to_string())
    });
    let account_id = thread
        .join()
        .map_err(|_panic| color_eyre::eyre::eyre!("Codex authentication worker panicked"))??;
    Ok(UpstreamCredentials::Codex {
        controller,
        account_id,
    })
}

fn openai_allowed_paths() -> BTreeSet<String> {
    BTreeSet::from([
        String::from("/v1/chat/completions"),
        String::from("/v1/models"),
        String::from("/v1/responses"),
    ])
}

pub(super) fn codex_allowed_paths() -> BTreeSet<String> {
    BTreeSet::from([
        String::from("/backend-api/codex/responses"),
        String::from("/backend-api/codex/models"),
    ])
}

#[cfg(test)]
mod tests {
    use std::fs;

    use color_eyre::eyre::ensure;

    use super::*;

    #[test]
    fn chat_completions_credentials_split_the_provider_base_url() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-chat-credentials-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&root)?;
        let source = root.join("providers.toml");
        for (base_url, upstream, base_path) in [
            (
                "http://127.0.0.1:10014/v1/",
                "http://127.0.0.1:10014",
                "/v1",
            ),
            ("https://example.invalid", "https://example.invalid", ""),
        ] {
            fs::write(
                &source,
                format!(
                    "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\napi_key = 'key'\nbase_url = '{base_url}'\n"
                ),
            )?;
            let credentials = ProxyCredentialRequest::ChatCompletions {
                provider: KraaiProviderConfigRequest::new(source.clone(), None),
            }
            .resolve()?;
            ensure!(credentials.upstream() == upstream);
            ensure!(credentials.base_path() == base_path);
            ensure!(
                credentials.allowed_paths()
                    == BTreeSet::from([
                        format!("{base_path}/chat/completions"),
                        format!("{base_path}/models"),
                    ])
            );
            ensure!(credentials.credential_source() == "kraai-provider:local");
        }
        for base_url in [
            "http://127.0.0.1/v1?x=1",
            "http://user:secret@127.0.0.1/v1",
            "http://user@127.0.0.1/v1",
        ] {
            fs::write(
                &source,
                format!(
                    "[[provider]]\nid = 'local'\ntype = 'openai-chat-completions'\napi_key = 'key'\nbase_url = '{base_url}'\n"
                ),
            )?;
            ensure!(
                ProxyCredentialRequest::ChatCompletions {
                    provider: KraaiProviderConfigRequest::new(source.clone(), None),
                }
                .resolve()
                .is_err()
            );
        }
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
