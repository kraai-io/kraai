use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_runtime::{CreateSessionRequest, RuntimeHandle};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error;

#[derive(Clone, Debug)]
pub struct Options {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Model {
    pub(crate) provider: String,
    pub(crate) model: String,
}

impl Model {
    pub(crate) fn id(&self) -> String {
        format!("{}:{}:{}", self.provider.len(), self.provider, self.model)
    }
}

pub(crate) struct Session {
    pub(crate) ready: AtomicBool,
    pub(crate) turn: Arc<Mutex<()>>,
    pub(crate) cancellation: std::sync::Mutex<Option<CancellationToken>>,
}

impl Session {
    pub(crate) fn begin_turn(self: &Arc<Self>) -> Result<ActiveTurn> {
        let permit = self
            .turn
            .clone()
            .try_lock_owned()
            .map_err(|_busy| error::invalid("A prompt is already active in this session"))?;
        let token = CancellationToken::new();
        *self
            .cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(token.clone());
        Ok(ActiveTurn {
            session: self.clone(),
            token,
            _permit: permit,
        })
    }
    pub(crate) fn cancel(&self) {
        if let Some(token) = self
            .cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            token.cancel();
        }
    }
}

pub(crate) struct ActiveTurn {
    pub(crate) session: Arc<Session>,
    pub(crate) token: CancellationToken,
    _permit: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.session
            .cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

pub(crate) async fn selected_model(runtime: &RuntimeHandle, id: &acp::SessionId) -> Result<Model> {
    let selected = runtime
        .get_session_model(id.to_string())
        .await
        .map_err(error::runtime)?
        .ok_or_else(|| error::internal("Session has no selected model"))?;
    Ok(Model {
        provider: selected.provider_id.to_string(),
        model: selected.model_id.to_string(),
    })
}

impl Model {
    pub(crate) fn selection(&self) -> Result<kraai_types::ModelSelection> {
        Ok(kraai_types::ModelSelection {
            provider_id: kraai_types::ProviderId::try_new(self.provider.clone())
                .map_err(error::invalid)?,
            model_id: kraai_types::ModelId::try_new(self.model.clone()).map_err(error::invalid)?,
        })
    }
}

pub(crate) async fn select_model(
    runtime: &RuntimeHandle,
    options: &Options,
    saved: Option<kraai_types::ModelSelection>,
) -> Result<Model> {
    let providers: BTreeMap<_, _> = runtime
        .list_models()
        .await
        .map_err(error::runtime)?
        .into_iter()
        .collect();
    if let Some(saved) = saved {
        let matches_overrides = options
            .provider
            .as_ref()
            .is_none_or(|id| id == saved.provider_id.as_str())
            && options
                .model
                .as_ref()
                .is_none_or(|id| id == saved.model_id.as_str());
        if matches_overrides {
            if providers
                .get(saved.provider_id.as_str())
                .is_some_and(|models| {
                    models
                        .iter()
                        .any(|model| model.id == saved.model_id.as_str())
                })
            {
                return Ok(Model {
                    provider: saved.provider_id.to_string(),
                    model: saved.model_id.to_string(),
                });
            }
            if options.provider.is_none() && options.model.is_none() {
                return Err(error::invalid(
                    "Saved model is no longer configured. Select an explicit --provider and --model to load this session.",
                ));
            }
        }
    }
    for (provider, mut models) in providers {
        if options
            .provider
            .as_ref()
            .is_some_and(|selected| selected != &provider)
        {
            continue;
        }
        models.sort_by(|a, b| a.id.cmp(&b.id));
        for model in models {
            if options
                .model
                .as_ref()
                .is_none_or(|selected| selected == &model.id)
            {
                return Ok(Model {
                    provider,
                    model: model.id,
                });
            }
        }
    }
    Err(error::invalid(
        "No matching configured model. Configure Kraai providers and select --provider and --model.",
    ))
}

pub(crate) async fn create(
    runtime: &RuntimeHandle,
    options: &Options,
    request: acp::NewSessionRequest,
) -> Result<(String, Arc<Session>)> {
    if !request.cwd.is_absolute() {
        return Err(error::invalid("Session cwd must be absolute"));
    }
    let cwd = tokio::fs::canonicalize(request.cwd)
        .await
        .map_err(|error| crate::error::invalid(error.to_string()))?;
    let mcp = crate::mcp::config(request.mcp_servers, &cwd)?;
    let model = select_model(runtime, options, None).await?;
    let id = runtime
        .create_session_with(CreateSessionRequest {
            workspace_dir: Some(cwd.to_string_lossy().into_owned()),
            profile_id: options.profile.clone(),
        })
        .await
        .map_err(error::runtime)?;
    let configured = async {
        runtime
            .set_session_model(id.clone(), model.selection()?)
            .await
            .map_err(error::runtime)?;
        runtime
            .set_session_mcp_servers(id.clone(), mcp)
            .await
            .map_err(error::runtime)
    }
    .await;
    if let Err(error) = configured {
        runtime
            .delete_session(id.clone())
            .await
            .map_err(crate::error::runtime)?;
        return Err(error);
    }
    Ok((
        id,
        Arc::new(Session {
            ready: AtomicBool::new(true),
            turn: Arc::default(),
            cancellation: Default::default(),
        }),
    ))
}
