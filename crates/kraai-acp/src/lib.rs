#![forbid(unsafe_code)]

mod commands;
mod config;
mod content;
mod error;
mod history;
mod prompt;
mod session;

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use agent_client_protocol::{
    Agent, ConnectTo, Result,
    schema::{ProtocolVersion, v1 as acp},
};
use kraai_runtime::RuntimeHandle;
use tokio::sync::Mutex;

pub use session::Options;

struct Server {
    runtime: RuntimeHandle,
    options: Options,
    initialized: AtomicBool,
    sessions: Mutex<HashMap<String, Arc<session::Session>>>,
}

impl Server {
    fn require_initialized(&self) -> Result<()> {
        if self.initialized.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(error::invalid("Initialize the connection first"))
        }
    }

    async fn session(&self, id: &acp::SessionId) -> Result<Arc<session::Session>> {
        self.require_initialized()?;
        self.sessions
            .lock()
            .await
            .get(id.0.as_ref())
            .filter(|session| session.ready.load(Ordering::Acquire))
            .cloned()
            .ok_or_else(|| error::invalid("Unknown session"))
    }

    async fn load(
        &self,
        request: acp::LoadSessionRequest,
        connection: &agent_client_protocol::ConnectionTo<agent_client_protocol::Client>,
    ) -> Result<acp::LoadSessionResponse> {
        self.require_initialized()?;
        session::require_no_mcp(&request.mcp_servers)?;
        if !request.cwd.is_absolute() {
            return Err(error::invalid("Session cwd must be absolute"));
        }
        let cwd = tokio::fs::canonicalize(&request.cwd)
            .await
            .map_err(|error| crate::error::invalid(error.to_string()))?;
        let id = request.session_id.to_string();
        let existing = self
            .runtime
            .list_sessions()
            .await
            .map_err(error::runtime)?
            .into_iter()
            .find(|session| session.id == id)
            .ok_or_else(|| error::invalid("Unknown session"))?;
        if std::path::Path::new(&existing.workspace_dir) != cwd {
            return Err(error::invalid(
                "cwd does not match the stored session workspace",
            ));
        }
        let model = session::select_model(&self.runtime, &self.options).await?;
        let session = self
            .sessions
            .lock()
            .await
            .entry(id.clone())
            .or_insert_with(|| {
                Arc::new(session::Session {
                    ready: AtomicBool::new(false),
                    model: Mutex::new(model),
                    turn: Arc::default(),
                    cancellation: Default::default(),
                })
            })
            .clone();
        let _turn = session
            .turn
            .try_lock()
            .map_err(|_busy| error::invalid("Session has an active prompt"))?;
        self.runtime
            .load_session(id.clone())
            .await
            .map_err(error::runtime)?;
        let snapshot = self
            .runtime
            .get_session_snapshot(id.clone())
            .await
            .map_err(error::runtime)?;
        if snapshot.session.is_running {
            return Err(error::invalid("Session is still running"));
        }
        let model = session.model.lock().await.clone();
        let config = config::options(&self.runtime, &model, &id).await?;
        history::replay(&self.runtime, &snapshot, connection).await?;
        commands::advertise(connection, request.session_id)?;
        session.ready.store(true, Ordering::Release);
        Ok(acp::LoadSessionResponse::new().config_options(config))
    }
}

pub async fn serve(
    runtime: RuntimeHandle,
    options: Options,
    transport: impl ConnectTo<Agent>,
) -> Result<()> {
    let server = Arc::new(Server {
        runtime,
        options,
        initialized: AtomicBool::new(false),
        sessions: Mutex::new(HashMap::new()),
    });
    let initialize = server.clone();
    let create = server.clone();
    let prompt = server.clone();
    let load = server.clone();
    let configure = server.clone();
    let cancel = server;
    Agent
        .builder()
        .name("kraai")
        .on_receive_request(
            async move |_: acp::InitializeRequest, responder, _cx| {
                if initialize.initialized.swap(true, Ordering::AcqRel) {
                    return responder
                        .respond_with_error(error::invalid("Connection is already initialized"));
                }
                responder.respond(
                    acp::InitializeResponse::new(ProtocolVersion::V1)
                        .agent_info(acp::Implementation::new("kraai", env!("CARGO_PKG_VERSION")))
                        .agent_capabilities(
                            acp::AgentCapabilities::new()
                                .load_session(true)
                                .prompt_capabilities(acp::PromptCapabilities::new().image(true)),
                        ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest, responder, cx| {
                let server = create.clone();
                let connection = cx.clone();
                cx.spawn(async move {
                    let result = async {
                        server.require_initialized()?;
                        let (id, session) =
                            session::create(&server.runtime, &server.options, request).await?;
                        let model = session.model.lock().await.clone();
                        let config = config::options(&server.runtime, &model, &id).await?;
                        commands::advertise(&connection, acp::SessionId::new(id.clone()))?;
                        server.sessions.lock().await.insert(id.clone(), session);
                        Ok(acp::NewSessionResponse::new(id).config_options(config))
                    }
                    .await;
                    responder.respond_with_result(result)
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::LoadSessionRequest, responder, cx| {
                let server = load.clone();
                let connection = cx.clone();
                cx.spawn(async move {
                    responder.respond_with_result(server.load(request, &connection).await)
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::SetSessionConfigOptionRequest, responder, cx| {
                let server = configure.clone();
                let connection = cx.clone();
                cx.spawn(async move {
                    let result = async {
                        let session = server.session(&request.session_id).await?;
                        let _turn = session.turn.try_lock().map_err(|_busy| {
                            error::invalid("Cannot change settings during a prompt")
                        })?;
                        let config = config::set(
                            &server.runtime,
                            &session,
                            request.session_id.0.as_ref(),
                            request.config_id.0.as_ref(),
                            request.value,
                        )
                        .await?;
                        config::notify(&connection, request.session_id, config.clone())?;
                        Ok(acp::SetSessionConfigOptionResponse::new(config))
                    }
                    .await;
                    responder.respond_with_result(result)
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::PromptRequest, responder, cx| {
                let server = prompt.clone();
                let turn = match server
                    .session(&request.session_id)
                    .await
                    .and_then(|session| session.begin_turn())
                {
                    Ok(turn) => turn,
                    Err(error) => return responder.respond_with_error(error),
                };
                let connection = cx.clone();
                cx.spawn(async move {
                    let result = prompt::run(&server.runtime, turn, request, connection).await;
                    responder.respond_with_result(result)
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |request: acp::CancelNotification, _cx| {
                if let Ok(session) = cancel.session(&request.session_id).await {
                    session.cancel();
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(transport)
        .await
}
