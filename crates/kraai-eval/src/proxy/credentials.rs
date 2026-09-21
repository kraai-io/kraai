use std::collections::BTreeSet;

use color_eyre::eyre::{Context, Result, bail};
use kraai_provider_openai_codex::{OpenAiCodexAuthController, OpenAiCodexAuthControllerOptions};

const OPENAI_UPSTREAM: &str = "https://api.openai.com";
const CHATGPT_UPSTREAM: &str = "https://chatgpt.com";

#[derive(Debug, Clone)]
pub(super) enum ProxyCredentialRequest {
    OpenAiApiKey { credential_env: String },
    CodexSubscription,
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
}

impl UpstreamCredentials {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => "openai",
            Self::Codex { .. } => "openai-codex",
        }
    }

    pub(super) fn upstream(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => OPENAI_UPSTREAM,
            Self::Codex { .. } => CHATGPT_UPSTREAM,
        }
    }

    pub(super) fn base_path(&self) -> &'static str {
        match self {
            Self::OpenAiApiKey { .. } => "/v1",
            Self::Codex { .. } => "/backend-api",
        }
    }

    pub(super) fn allowed_paths(&self) -> BTreeSet<String> {
        match self {
            Self::OpenAiApiKey { .. } => openai_allowed_paths(),
            Self::Codex { .. } => codex_allowed_paths(),
        }
    }

    pub(super) fn credential_source(&self) -> String {
        match self {
            Self::OpenAiApiKey { credential_env, .. } => format!("env:{credential_env}"),
            Self::Codex { account_id, .. } => format!("codex-account:{account_id}"),
        }
    }

    pub(super) fn fingerprint(&self) -> String {
        let material = match self {
            Self::OpenAiApiKey { credential, .. } => credential.as_bytes(),
            Self::Codex { account_id, .. } => account_id.as_bytes(),
        };
        crate::cache::hash_chunks(&[material])
    }
}

fn codex_credentials() -> Result<UpstreamCredentials> {
    let auth_path =
        kraai_persistence::agent_state_root()?.join("provider-state/openai-codex/auth.json");
    let controller = OpenAiCodexAuthController::new_with_options(
        OpenAiCodexAuthControllerOptions::new(auth_path),
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
