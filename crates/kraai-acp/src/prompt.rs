use agent_client_protocol::{Result, v1::schema as acp};
use kraai_runtime::RuntimeHandle;
use tokio::sync::oneshot;

use crate::transport::Connection;
use crate::{commands, content, error, follow, session::ActiveTurn};

pub(crate) async fn run(
    runtime: &RuntimeHandle,
    turn: ActiveTurn,
    request: acp::PromptRequest,
    connection: Connection,
) -> Result<acp::PromptResponse> {
    let cancelled = &turn.token;
    if cancelled.is_cancelled() {
        return Ok(acp::PromptResponse::new(acp::StopReason::Cancelled));
    }
    let id = crate::session::runtime_id(&request.session_id)?;
    let model = crate::session::selected_model(runtime, &id).await?;
    if commands::is_undo(&request.prompt)? {
        commands::undo(runtime, &id, &connection).await?;
        return Ok(acp::PromptResponse::new(if cancelled.is_cancelled() {
            acp::StopReason::Cancelled
        } else {
            acp::StopReason::EndTurn
        }));
    }
    let message = content::prompt(runtime, request.prompt).await?;
    turn.session.apply_mcp(runtime, &id).await?;
    let (done, finished) = oneshot::channel();
    turn.session.follow(follow::Start {
        message,
        model,
        token: cancelled.clone(),
        done,
    })?;
    let reason = finished
        .await
        .map_err(|_closed| error::internal("Session stopped following runtime events"))??;
    Ok(acp::PromptResponse::new(reason))
}
