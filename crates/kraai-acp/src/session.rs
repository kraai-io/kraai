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
    pub(crate) model: Mutex<Model>,
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

pub(crate) async fn select_model(runtime: &RuntimeHandle, options: &Options) -> Result<Model> {
    let providers: BTreeMap<_, _> = runtime
        .list_models()
        .await
        .map_err(error::runtime)?
        .into_iter()
        .collect();
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
    require_no_mcp(&request.mcp_servers)?;
    let model = select_model(runtime, options).await?;
    let id = runtime
        .create_session_with(CreateSessionRequest {
            workspace_dir: Some(request.cwd.to_string_lossy().into_owned()),
            profile_id: options.profile.clone(),
        })
        .await
        .map_err(error::runtime)?;
    Ok((
        id,
        Arc::new(Session {
            ready: AtomicBool::new(true),
            model: Mutex::new(model),
            turn: Arc::default(),
            cancellation: Default::default(),
        }),
    ))
}

pub(crate) fn require_no_mcp(servers: &[acp::McpServer]) -> Result<()> {
    if servers.is_empty() {
        Ok(())
    } else {
        Err(error::invalid(
            "Attaching MCP servers through ACP is not implemented yet; use an empty mcpServers list",
        ))
    }
}

pub(crate) async fn config_options(
    runtime: &RuntimeHandle,
    selected: &Model,
) -> Result<Vec<acp::SessionConfigOption>> {
    let providers: BTreeMap<_, _> = runtime
        .list_models()
        .await
        .map_err(error::runtime)?
        .into_iter()
        .collect();
    let mut choices = Vec::new();
    for (provider, mut models) in providers {
        models.sort_by(|a, b| a.id.cmp(&b.id));
        for model in models {
            let id = Model {
                provider: provider.clone(),
                model: model.id.clone(),
            }
            .id();
            choices.push(acp::SessionConfigSelectOption::new(
                id,
                format!("{provider} / {}", model.name),
            ));
        }
    }
    Ok(vec![
        acp::SessionConfigOption::select("model", "Model", selected.id(), choices)
            .category(acp::SessionConfigOptionCategory::Model),
    ])
}

pub(crate) async fn set_model(
    runtime: &RuntimeHandle,
    session: &Session,
    value: acp::SessionConfigOptionValue,
) -> Result<Vec<acp::SessionConfigOption>> {
    let _turn = session
        .turn
        .try_lock()
        .map_err(|_busy| error::invalid("Cannot change models during a prompt"))?;
    let acp::SessionConfigOptionValue::ValueId { value } = value else {
        return Err(error::invalid("Model must be a string"));
    };
    let providers = runtime.list_models().await.map_err(error::runtime)?;
    let selected = providers
        .into_iter()
        .flat_map(|(provider, models)| {
            models.into_iter().map(move |model| Model {
                provider: provider.clone(),
                model: model.id,
            })
        })
        .find(|model| model.id() == value.0.as_ref())
        .ok_or_else(|| error::invalid("Unknown model"))?;
    let config = config_options(runtime, &selected).await?;
    *session.model.lock().await = selected;
    Ok(config)
}
