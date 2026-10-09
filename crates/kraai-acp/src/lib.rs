#![forbid(unsafe_code)]

mod commands;
mod config;
mod config_updates;
mod content;
mod error;
mod history;
mod mcp;
mod prompt;
mod session;
mod transport;

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use agent_client_protocol::{
    Agent, Result,
    schema::{ProtocolVersion, v1 as acp},
};
use config_updates::{ConfigUpdates, PreparedSession};
use kraai_runtime::RuntimeHandle;
use tokio::sync::Mutex;

pub use session::Options;
pub use transport::Stdio;

struct Server {
    budget: transport::Budget,
    runtime: RuntimeHandle,
    options: Options,
    initialized: AtomicBool,
    sessions: Mutex<BTreeMap<String, Arc<session::Session>>>,
    config_updated: Arc<ConfigUpdates>,
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
        connection: &transport::Connection,
    ) -> Result<PreparedSession<acp::LoadSessionResponse>> {
        self.require_initialized()?;
        if !request.cwd.is_absolute() {
            return Err(error::invalid("Session cwd must be absolute"));
        }
        let cwd = tokio::fs::canonicalize(&request.cwd)
            .await
            .map_err(|error| crate::error::invalid(error.to_string()))?;
        let mcp = mcp::config(request.mcp_servers, &cwd)?;
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
        let session = self
            .sessions
            .lock()
            .await
            .entry(id.clone())
            .or_insert_with(|| Arc::new(session::Session::new(self.config_updated.clone())))
            .clone();
        let _turn = session
            .turn
            .try_lock()
            .map_err(|_busy| error::invalid("Session has an active prompt"))?;
        let mut publication = config_updates::lock(&session).await;
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
        if !session.ready.load(Ordering::Acquire) {
            let saved = self
                .runtime
                .get_session_model(id.clone())
                .await
                .map_err(error::runtime)?;
            let model = session::select_model(&self.runtime, &self.options, saved).await?;
            self.runtime
                .set_session_model(id.clone(), model.selection()?)
                .await
                .map_err(error::runtime)?;
        }
        let config = config::options(&self.runtime, &id).await?;
        self.runtime
            .set_session_mcp_servers(id.clone(), mcp)
            .await
            .map_err(error::runtime)?;
        history::replay(&self.runtime, &snapshot, connection).await?;
        commands::advertise(connection, request.session_id).await?;
        *publication.current = Some(config.clone());
        drop(_turn);
        Ok(PreparedSession {
            response: acp::LoadSessionResponse::new().config_options(config),
            session,
            _publication: publication,
        })
    }
}

pub async fn serve(runtime: RuntimeHandle, options: Options, transport: Stdio) -> Result<()> {
    let server = Arc::new(Server {
        budget: transport.budget.clone(),
        runtime,
        options,
        initialized: AtomicBool::new(false),
        sessions: Mutex::new(BTreeMap::new()),
        config_updated: Arc::default(),
    });
    let initialize = server.clone();
    let create = server.clone();
    let prompt = server.clone();
    let load = server.clone();
    let configure = server.clone();
    let close = server.clone();
    let cancel = server.clone();
    Agent
        .builder()
        .name("kraai")
        .on_close(async move |_cx| {
            for session in close.sessions.lock().await.values() {
                session.cancel();
            }
            close.runtime.shutdown().await.map_err(error::runtime)
        })
        .on_receive_request(
            async move |_: acp::InitializeRequest, responder, cx| {
                if initialize.initialized.swap(true, Ordering::AcqRel) {
                    return responder
                        .respond_with_error(error::invalid("Connection is already initialized"));
                }
                let server = initialize.clone();
                let connection = transport::Connection::new(cx.clone(), server.budget.clone());
                let updates = server.runtime.subscribe();
                cx.spawn(async move {
                    tokio::select! {
                        biased;
                        () = connection.incoming_closed() => Ok(()),
                        result = config_updates::run(&server, &connection, updates) => result,
                    }
                })?;
                responder.respond(
                    acp::InitializeResponse::new(ProtocolVersion::V1)
                        .agent_info(acp::Implementation::new("kraai", env!("CARGO_PKG_VERSION")))
                        .agent_capabilities(
                            acp::AgentCapabilities::new()
                                .load_session(true)
                                .mcp_capabilities(acp::McpCapabilities::new().http(true))
                                .prompt_capabilities(acp::PromptCapabilities::new().image(true)),
                        ),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::NewSessionRequest, responder, cx| {
                let server = create.clone();
                let connection = transport::Connection::new(cx.clone(), server.budget.clone());
                cx.spawn(async move {
                    let result = async {
                        server.require_initialized()?;
                        let (id, session) = session::create(
                            &server.runtime,
                            &server.options,
                            request,
                            server.config_updated.clone(),
                        )
                        .await?;
                        let mut publication = config_updates::lock(&session).await;
                        let config = config::options(&server.runtime, &id).await?;
                        commands::advertise(&connection, acp::SessionId::new(id.clone())).await?;
                        *publication.current = Some(config.clone());
                        server
                            .sessions
                            .lock()
                            .await
                            .insert(id.clone(), session.clone());
                        Ok(PreparedSession {
                            response: acp::NewSessionResponse::new(id).config_options(config),
                            session,
                            _publication: publication,
                        })
                    }
                    .await;
                    match result {
                        Ok(prepared) => prepared.respond(|response| responder.respond(response)),
                        Err(error) => responder.respond_with_error(error),
                    }
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::LoadSessionRequest, responder, cx| {
                let server = load.clone();
                let connection = transport::Connection::new(cx.clone(), server.budget.clone());
                cx.spawn(async move {
                    match server.load(request, &connection).await {
                        Ok(prepared) => prepared.respond(|response| responder.respond(response)),
                        Err(error) => responder.respond_with_error(error),
                    }
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::SetSessionConfigOptionRequest, responder, cx| {
                let server = configure.clone();
                let connection = transport::Connection::new(cx.clone(), server.budget.clone());
                cx.spawn(async move {
                    let session = match server.session(&request.session_id).await {
                        Ok(session) => session,
                        Err(error) => return responder.respond_with_error(error),
                    };
                    let _turn = match session.turn.try_lock() {
                        Ok(turn) => turn,
                        Err(_busy) => {
                            return responder.respond_with_error(error::invalid(
                                "Cannot change settings during a prompt",
                            ));
                        }
                    };
                    let mut publication = config_updates::lock(&session).await;
                    let result = async {
                        let config = config::set(
                            &server.runtime,
                            request.session_id.0.as_ref(),
                            request.config_id.0.as_ref(),
                            request.value,
                        )
                        .await?;
                        config::notify(&connection, request.session_id, config.clone())?;
                        *publication.current = Some(config.clone());
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
                let connection = transport::Connection::new(cx.clone(), server.budget.clone());
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
