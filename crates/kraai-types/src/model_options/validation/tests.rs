use super::*;
use crate::{ModelOptionChoice, ModelOptionCondition};

fn effort(values: &[&str]) -> ModelOptionDefinition {
    ModelOptionDefinition {
        id: "effort".into(),
        label: "Reasoning effort".into(),
        description: None,
        required: true,
        active_when: None,
        binding: Some(ModelOptionBinding::Body {
            path: "/reasoning/effort".into(),
        }),
        kind: ModelOptionKind::Choice {
            choices: values
                .iter()
                .map(|id| ModelOptionChoice {
                    id: (*id).into(),
                    label: (*id).into(),
                    patch: Default::default(),
                })
                .collect(),
        },
    }
}

#[test]
fn required_values_are_explicit_and_model_specific() {
    let first = vec![effort(&["light", "future-effort"])];
    let second = vec![effort(&["medium", "high"])];
    let values = ModelOptionValues::from([(
        "effort".into(),
        ModelOptionValue::Choice("future-effort".into()),
    )]);
    assert!(validate_model_options(&first, &values).is_ok());
    assert!(validate_model_options(&second, &values).is_err());
    assert!(validate_model_options(&first, &Default::default()).is_err());
    assert!(validate_model_option_values(&first, &Default::default(), false).is_ok());
}

#[test]
fn conditional_budget_checks_presence_type_and_bounds() {
    let toggle = ModelOptionDefinition {
        id: "thinking".into(),
        label: "Thinking".into(),
        description: None,
        required: true,
        active_when: None,
        binding: None,
        kind: ModelOptionKind::Boolean {
            enabled: Default::default(),
            disabled: Default::default(),
        },
    };
    let budget = ModelOptionDefinition {
        id: "budget".into(),
        label: "Budget".into(),
        description: None,
        required: true,
        active_when: Some(ModelOptionCondition {
            option: "thinking".into(),
            value: ModelOptionValue::Boolean(true),
        }),
        binding: Some(ModelOptionBinding::Body {
            path: "/thinking/budget_tokens".into(),
        }),
        kind: ModelOptionKind::Integer {
            min: Some(1024),
            max: Some(8192),
        },
    };
    let definitions = vec![toggle, budget];
    let mut values =
        ModelOptionValues::from([("thinking".into(), ModelOptionValue::Boolean(false))]);
    assert!(validate_model_options(&definitions, &values).is_ok());
    values.insert("thinking".into(), ModelOptionValue::Boolean(true));
    assert!(validate_model_options(&definitions, &values).is_err());
    for value in [
        ModelOptionValue::Integer(1023),
        ModelOptionValue::Integer(8193),
        ModelOptionValue::Choice("2048".into()),
    ] {
        values.insert("budget".into(), value);
        assert!(validate_model_options(&definitions, &values).is_err());
    }
    values.insert("budget".into(), ModelOptionValue::Integer(2048));
    assert!(validate_model_options(&definitions, &values).is_ok());
    values.insert("thinking".into(), ModelOptionValue::Boolean(false));
    assert!(validate_model_options(&definitions, &values).is_err());
}

#[test]
fn invalid_descriptors_and_unknown_settings_are_rejected() {
    let mut definition = effort(&["low", "low"]);
    assert!(
        validate_model_option_values(&[definition.clone()], &Default::default(), false).is_err()
    );
    definition = effort(&["low"]);
    definition.binding = Some(ModelOptionBinding::Body {
        path: "/bad~2path".into(),
    });
    assert!(validate_model_option_values(&[definition], &Default::default(), false).is_err());
    let mut first = effort(&["low"]);
    first.active_when = Some(ModelOptionCondition {
        option: "effort".into(),
        value: ModelOptionValue::Choice("low".into()),
    });
    assert!(validate_model_option_values(&[first], &Default::default(), false).is_err());
    let values = ModelOptionValues::from([("unknown".into(), ModelOptionValue::Integer(1))]);
    assert!(validate_model_option_values(&[effort(&["low"])], &values, false).is_err());
}

#[test]
fn assignment_parsing_preserves_exact_ids_and_requires_explicit_values() {
    let definitions = vec![effort(&["future=value", "low"])];
    let inputs = vec!["effort=future=value".into()];
    let result = crate::parse_model_option_assignments(&definitions, &inputs, Default::default());
    assert_eq!(
        result,
        Ok(ModelOptionValues::from([(
            "effort".into(),
            ModelOptionValue::Choice("future=value".into())
        )]))
    );
    for invalid in [
        vec!["effort".into()],
        vec!["unknown=low".into()],
        vec!["effort=low".into(), "effort=low".into()],
    ] {
        assert!(
            crate::parse_model_option_assignments(&definitions, &invalid, Default::default())
                .is_err()
        );
    }
    assert!(
        crate::parse_model_option_assignments(
            &definitions,
            &["effort=other".into()],
            Default::default()
        )
        .is_err()
    );
}

#[test]
fn assignments_remove_inactive_descendants_and_reject_new_inactive_values() {
    let mut root = effort(&["on", "off"]);
    root.id = "root".into();
    let mut child = effort(&["on", "off"]);
    child.id = "child".into();
    child.active_when = Some(ModelOptionCondition {
        option: "root".into(),
        value: ModelOptionValue::Choice("on".into()),
    });
    let mut leaf = effort(&["deep"]);
    leaf.id = "leaf".into();
    leaf.active_when = Some(ModelOptionCondition {
        option: "child".into(),
        value: ModelOptionValue::Choice("on".into()),
    });
    let definitions = vec![root, child, leaf];
    let existing = ModelOptionValues::from([
        ("root".into(), ModelOptionValue::Choice("on".into())),
        ("child".into(), ModelOptionValue::Choice("on".into())),
        ("leaf".into(), ModelOptionValue::Choice("deep".into())),
    ]);
    let updated =
        crate::parse_model_option_assignments(&definitions, &["root=off".into()], existing.clone());
    assert_eq!(
        updated,
        Ok(ModelOptionValues::from([(
            "root".into(),
            ModelOptionValue::Choice("off".into())
        ),]))
    );
    assert!(
        crate::parse_model_option_assignments(
            &definitions,
            &["root=off".into(), "leaf=deep".into()],
            existing,
        )
        .is_err()
    );
}
