use agent_client_protocol::{Result, v1::schema as acp};
use kraai_runtime::RuntimeHandle;

use crate::error;

pub(super) fn model_option(
    option: &kraai_types::ModelOptionDefinition,
    value: Option<&kraai_types::ModelOptionValue>,
) -> acp::SessionConfigOption {
    use kraai_types::{ModelOptionKind, ModelOptionValue};
    let id = format!("option:{}", option.id);
    if option.required
        && let (ModelOptionKind::Boolean { .. }, Some(ModelOptionValue::Boolean(value))) =
            (&option.kind, value)
    {
        return acp::SessionConfigOption::boolean(id, &option.label, *value)
            .description(option.description.clone());
    }
    let mut choices = vec![acp::SessionConfigSelectOption::new(
        "",
        if option.required {
            "Choose a value"
        } else {
            "Unset"
        },
    )];
    match &option.kind {
        ModelOptionKind::Choice { choices: values } => {
            choices.extend(values.iter().map(|choice| {
                acp::SessionConfigSelectOption::new(choice.id.clone(), &choice.label)
            }));
            if let Some(ModelOptionValue::Choice(value)) = value
                && values.iter().all(|choice| &choice.id != value)
            {
                choices.push(acp::SessionConfigSelectOption::new(value.clone(), value));
            }
        }
        ModelOptionKind::Boolean { .. } => choices.extend([
            acp::SessionConfigSelectOption::new("false", "false"),
            acp::SessionConfigSelectOption::new("true", "true"),
        ]),
        ModelOptionKind::Integer { .. } => {
            if let Some(ModelOptionValue::Integer(value)) = value {
                choices.push(acp::SessionConfigSelectOption::new(
                    value.to_string(),
                    value.to_string(),
                ));
            }
        }
    }
    let selected = match value {
        Some(ModelOptionValue::Choice(value)) => value.clone(),
        Some(ModelOptionValue::Boolean(value)) => value.to_string(),
        Some(ModelOptionValue::Integer(value)) => value.to_string(),
        None => String::new(),
    };
    acp::SessionConfigOption::select(id, &option.label, selected, choices)
        .description(option.description.clone())
}

pub(crate) async fn set_model_option(
    runtime: &RuntimeHandle,
    session_id: &kraai_types::SessionId,
    id: &str,
    input: &str,
) -> Result<()> {
    let models = runtime.list_models().await.map_err(error::runtime)?;
    let mut selected =
        crate::session::selected_model_with_models(runtime, session_id, &models).await?;
    let model = models
        .get(&selected.provider)
        .and_then(|models| models.iter().find(|model| model.id == selected.model))
        .ok_or_else(|| error::invalid("Selected model is unavailable"))?;
    selected.options = kraai_types::parse_model_option_assignments(
        &model.options,
        &[format!("{id}={input}")],
        selected.options,
    )
    .map_err(error::invalid)?;
    runtime
        .set_session_model(session_id.clone(), selected.selection()?)
        .await
        .map_err(error::runtime)
}

#[cfg(test)]
mod tests;
