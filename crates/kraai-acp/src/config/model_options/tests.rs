use super::*;
use serde_json::Value;

#[test]
fn unset_boolean_requires_an_explicit_choice() {
    let definition = serde_json::from_value(
        serde_json::json!({"id":"thinking","label":"Thinking","type":"boolean","required":true}),
    )
    .unwrap();
    let option = serde_json::to_value(model_option(&definition, None)).unwrap();
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
    ))
    .unwrap();
    assert_eq!(chosen.get("type").and_then(Value::as_str), Some("boolean"));
    assert_eq!(
        chosen.get("currentValue").and_then(Value::as_bool),
        Some(false)
    );
}

#[test]
fn optional_controls_keep_a_clear_choice_after_selection() {
    for (definition, value, selected) in [
        (
            serde_json::json!({"id":"mode","label":"Mode","type":"choice","choices":[{"id":"custom","label":"Custom"}]}),
            kraai_types::ModelOptionValue::Choice("custom".into()),
            "custom",
        ),
        (
            serde_json::json!({"id":"thinking","label":"Thinking","type":"boolean"}),
            kraai_types::ModelOptionValue::Boolean(false),
            "false",
        ),
        (
            serde_json::json!({"id":"budget","label":"Budget","type":"integer","min":0,"max":100,"binding":{"type":"body","path":"/budget"}}),
            kraai_types::ModelOptionValue::Integer(0),
            "0",
        ),
    ] {
        let definition = serde_json::from_value(definition).unwrap();
        let option = serde_json::to_value(model_option(&definition, Some(&value))).unwrap();
        assert_eq!(option.get("type").and_then(Value::as_str), Some("select"));
        assert_eq!(
            option.get("currentValue").and_then(Value::as_str),
            Some(selected)
        );
        assert_eq!(
            option.pointer("/options/0/value"),
            Some(&serde_json::json!(""))
        );
        assert_eq!(
            option.pointer("/options/0/name"),
            Some(&serde_json::json!("Unset"))
        );
    }
}
