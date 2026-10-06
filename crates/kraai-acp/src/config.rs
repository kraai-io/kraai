use std::collections::BTreeMap;

use agent_client_protocol::{Client, ConnectionTo, Result, schema::v1 as acp};
use kraai_runtime::RuntimeHandle;

use crate::{
    error,
    session::{Model, Session},
};

pub(crate) async fn options(
    runtime: &RuntimeHandle,
    selected: &Model,
    session_id: &str,
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
    let profiles = runtime
        .list_agent_profiles(session_id.to_owned())
        .await
        .map_err(error::runtime)?;
    let selected_profile = profiles
        .selected_profile_id
        .ok_or_else(|| error::internal("Session has no selected profile"))?;
    let profile_choices = profiles
        .profiles
        .into_iter()
        .map(|profile| {
            acp::SessionConfigSelectOption::new(profile.id, profile.display_name)
                .description(profile.description)
        })
        .collect::<Vec<_>>();
    Ok(vec![
        acp::SessionConfigOption::select("model", "Model", selected.id(), choices)
            .category(acp::SessionConfigOptionCategory::Model),
        acp::SessionConfigOption::select(
            "profile",
            "Agent profile",
            selected_profile,
            profile_choices,
        )
        .category(acp::SessionConfigOptionCategory::Mode),
    ])
}

pub(crate) async fn set(
    runtime: &RuntimeHandle,
    session: &Session,
    session_id: &str,
    config_id: &str,
    value: acp::SessionConfigOptionValue,
) -> Result<Vec<acp::SessionConfigOption>> {
    let acp::SessionConfigOptionValue::ValueId { value } = value else {
        return Err(error::invalid("Configuration value must be a string"));
    };
    match config_id {
        "model" => {
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
            let config = options(runtime, &selected, session_id).await?;
            *session.model.lock().await = selected;
            Ok(config)
        }
        "profile" => {
            set_profile(runtime, session_id, value.0.as_ref()).await?;
            let selected = session.model.lock().await.clone();
            options(runtime, &selected, session_id).await
        }
        _ => Err(error::invalid("Unknown configuration option")),
    }
}

pub(crate) async fn set_profile(
    runtime: &RuntimeHandle,
    session_id: &str,
    profile_id: &str,
) -> Result<()> {
    runtime
        .set_session_profile(session_id.to_owned(), profile_id.to_owned())
        .await
        .map_err(error::runtime)
}

pub(crate) fn notify(
    connection: &ConnectionTo<Client>,
    id: acp::SessionId,
    config: Vec<acp::SessionConfigOption>,
) -> Result<()> {
    connection.send_notification(acp::SessionNotification::new(
        id,
        acp::SessionUpdate::ConfigOptionUpdate(acp::ConfigOptionUpdate::new(config)),
    ))
}
