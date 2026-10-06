use agent_client_protocol::{Client, ConnectionTo, Result, schema::v1 as acp};
use kraai_runtime::RuntimeHandle;

use crate::{config, content, error, session::Session};

pub(crate) enum Command<'a> {
    Agent(Option<&'a str>),
    Continue,
}

pub(crate) fn parse(prompt: &[acp::ContentBlock]) -> Result<Option<Command<'_>>> {
    let [acp::ContentBlock::Text(text)] = prompt else {
        return Ok(None);
    };
    let mut words = text.text.split_whitespace();
    match words.next() {
        Some("/agent") => {
            let profile = words.next();
            if words.next().is_some() {
                return Err(error::invalid("Usage: /agent [profile-id]"));
            }
            Ok(Some(Command::Agent(profile)))
        }
        Some("/continue") => {
            if words.next().is_some() {
                return Err(error::invalid("Usage: /continue"));
            }
            Ok(Some(Command::Continue))
        }
        _ => Ok(None),
    }
}

pub(crate) fn advertise(connection: &ConnectionTo<Client>, id: acp::SessionId) -> Result<()> {
    connection.send_notification(acp::SessionNotification::new(
        id,
        acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(vec![
            acp::AvailableCommand::new("agent", "List agent profiles or select a profile").input(
                acp::AvailableCommandInput::Unstructured(acp::UnstructuredCommandInput::new(
                    "profile-id (optional)",
                )),
            ),
            acp::AvailableCommand::new(
                "continue",
                "Resume the conversation using the selected model",
            ),
        ])),
    ))
}

pub(crate) async fn agent(
    runtime: &RuntimeHandle,
    session: &Session,
    id: &acp::SessionId,
    profile: Option<&str>,
    connection: &ConnectionTo<Client>,
) -> Result<()> {
    let text = if let Some(profile) = profile {
        config::set_profile(runtime, id.0.as_ref(), profile).await?;
        let model = session.model.lock().await.clone();
        let options = config::options(runtime, &model, id.0.as_ref()).await?;
        config::notify(connection, id.clone(), options)?;
        format!("Selected agent profile: {profile}")
    } else {
        let profiles = runtime
            .list_agent_profiles(id.to_string())
            .await
            .map_err(error::runtime)?;
        profiles
            .profiles
            .into_iter()
            .map(|profile| {
                let selected = if profiles.selected_profile_id.as_deref() == Some(&profile.id) {
                    " (selected)"
                } else {
                    ""
                };
                format!(
                    "{}: {}{selected}\n{}",
                    profile.id, profile.display_name, profile.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    connection.send_notification(acp::SessionNotification::new(
        id.clone(),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(content::text(text))),
    ))
}
