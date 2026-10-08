use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_runtime::RuntimeHandle;

use crate::{error, session::Model};

pub(super) async fn providers_for_model(
    runtime: &RuntimeHandle,
    selected: &Model,
) -> Result<Option<kraai_runtime::Model>> {
    Ok(runtime
        .list_models()
        .await
        .map_err(error::runtime)?
        .remove(&selected.provider)
        .and_then(|models| models.into_iter().find(|model| model.id == selected.model)))
}

pub(super) fn model_option(
    option: &kraai_types::ModelOptionDefinition,
    value: Option<&kraai_types::ModelOptionValue>,
) -> acp::SessionConfigOption {
    use kraai_types::{ModelOptionKind, ModelOptionValue};
    let id = format!("option:{}", option.id);
    if let (ModelOptionKind::Boolean { .. }, Some(ModelOptionValue::Boolean(value))) =
        (&option.kind, value)
    {
        return acp::SessionConfigOption::boolean(id, &option.label, *value)
            .description(option.description.clone());
    }
    let mut choices = vec![acp::SessionConfigSelectOption::new("", "Choose a value")];
    match &option.kind {
        ModelOptionKind::Choice { choices: values } => {
            choices.extend(values.iter().map(|choice| {
                acp::SessionConfigSelectOption::new(choice.id.clone(), &choice.label)
            }))
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
    let description = if matches!(option.kind, ModelOptionKind::Integer { .. }) {
        Some(format!("Set with /option {} <integer>", option.id))
    } else {
        option.description.clone()
    };
    acp::SessionConfigOption::select(id, &option.label, selected, choices).description(description)
}

pub(crate) async fn set_model_option(
    runtime: &RuntimeHandle,
    session_id: &str,
    id: &str,
    input: &str,
) -> Result<()> {
    let mut selected =
        crate::session::selected_model(runtime, &acp::SessionId::new(session_id)).await?;
    let model = providers_for_model(runtime, &selected)
        .await?
        .ok_or_else(|| error::invalid("Selected model is unavailable"))?;
    selected.options = kraai_types::parse_model_option_assignments(
        &model.options,
        &[format!("{id}={input}")],
        selected.options,
    )
    .map_err(error::invalid)?;
    runtime
        .set_session_model(session_id.to_owned(), selected.selection()?)
        .await
        .map_err(error::runtime)
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "config option tests assert after fallible descriptor parsing"
)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn unset_boolean_requires_an_explicit_choice() -> color_eyre::Result<()> {
        let definition = serde_json::from_value(
            serde_json::json!({"id":"thinking","label":"Thinking","type":"boolean","required":true}),
        )?;
        let option = serde_json::to_value(model_option(&definition, None))?;
        assert_eq!(option.get("type").and_then(Value::as_str), Some("select"));
        assert_eq!(option.get("currentValue").and_then(Value::as_str), Some(""));
        assert_eq!(
            option
                .get("options")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(3)
        );
        let chosen = serde_json::to_value(model_option(
            &definition,
            Some(&kraai_types::ModelOptionValue::Boolean(false)),
        ))?;
        assert_eq!(chosen.get("type").and_then(Value::as_str), Some("boolean"));
        assert_eq!(
            chosen.get("currentValue").and_then(Value::as_bool),
            Some(false)
        );
        Ok(())
    }

    #[test]
    fn integer_options_explain_the_typed_setter() -> color_eyre::Result<()> {
        let definition = serde_json::from_value(
            serde_json::json!({"id":"budget","label":"Budget","type":"integer","min":10,"max":100,"binding":{"type":"body","path":"/budget"}}),
        )?;
        let option = serde_json::to_value(model_option(&definition, None))?;
        assert_eq!(option.get("currentValue").and_then(Value::as_str), Some(""));
        assert!(
            option
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|description| description.contains("/option budget <integer>"))
        );
        Ok(())
    }
}
