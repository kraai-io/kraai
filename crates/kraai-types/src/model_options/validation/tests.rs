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

fn optional_controls() -> Vec<ModelOptionDefinition> {
    let mut choice = effort(&["custom", "clear"]);
    choice.required = false;
    let mut toggle = choice.clone();
    toggle.id = "toggle".into();
    toggle.kind = ModelOptionKind::Boolean {
        enabled: Default::default(),
        disabled: Default::default(),
    };
    let mut budget = choice.clone();
    budget.id = "budget".into();
    budget.kind = ModelOptionKind::Integer {
        min: Some(0),
        max: Some(100),
    };
    vec![choice, toggle, budget]
}

#[test]
fn optional_assignments_clear_values_and_required_controls_reject_clears() {
    let definitions = optional_controls();
    let existing = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice("custom".into())),
        ("toggle".into(), ModelOptionValue::Boolean(false)),
        ("budget".into(), ModelOptionValue::Integer(0)),
    ]);
    for definition in &definitions {
        let mut expected = existing.clone();
        expected.remove(&definition.id);
        let input = vec![format!("{}=", definition.id)];
        assert_eq!(
            crate::parse_model_option_assignments(&definitions, &input, existing.clone()),
            Ok(expected)
        );
        assert_eq!(
            crate::parse_model_option_assignments(&definitions, &input, Default::default()),
            Ok(Default::default())
        );
        let mut required = definition.clone();
        required.required = true;
        assert!(
            crate::parse_model_option_assignments(&[required], &input, Default::default()).is_err()
        );
    }
    assert!(
        crate::parse_model_option_assignments(&definitions, &["unknown=".into()], existing)
            .is_err()
    );
    assert_eq!(
        crate::parse_model_option_assignments(
            &definitions,
            &["effort=clear".into()],
            Default::default()
        ),
        Ok(ModelOptionValues::from([(
            "effort".into(),
            ModelOptionValue::Choice("clear".into())
        )]))
    );
}

#[test]
fn clearing_optional_parent_removes_conditional_descendants() {
    let mut definitions = optional_controls();
    let mut child = effort(&["on"]);
    child.id = "child".into();
    child.active_when = Some(ModelOptionCondition {
        option: "toggle".into(),
        value: ModelOptionValue::Boolean(true),
    });
    let mut leaf = effort(&["deep"]);
    leaf.id = "leaf".into();
    leaf.active_when = Some(ModelOptionCondition {
        option: "child".into(),
        value: ModelOptionValue::Choice("on".into()),
    });
    definitions.extend([child, leaf]);
    let existing = ModelOptionValues::from([
        ("toggle".into(), ModelOptionValue::Boolean(true)),
        ("child".into(), ModelOptionValue::Choice("on".into())),
        ("leaf".into(), ModelOptionValue::Choice("deep".into())),
    ]);
    assert_eq!(
        crate::parse_model_option_assignments(&definitions, &["toggle=".into()], existing.clone()),
        Ok(Default::default())
    );
    assert!(
        crate::parse_model_option_assignments(
            &definitions,
            &["toggle=".into(), "leaf=deep".into()],
            existing
        )
        .is_err()
    );
}

#[test]
fn reconciliation_drops_incompatible_values_and_preserves_false_zero_and_custom_choices() {
    let mut definitions = optional_controls();
    let mut changed = effort(&["new"]);
    changed.id = "changed".into();
    let mut child = effort(&["on"]);
    child.id = "child".into();
    child.active_when = Some(ModelOptionCondition {
        option: "changed".into(),
        value: ModelOptionValue::Choice("new".into()),
    });
    let mut leaf = effort(&["deep"]);
    leaf.id = "leaf".into();
    leaf.active_when = Some(ModelOptionCondition {
        option: "child".into(),
        value: ModelOptionValue::Choice("on".into()),
    });
    let mut changed_type = effort(&["custom"]);
    changed_type.id = "changed_type".into();
    let mut outside_bounds = effort(&["custom"]);
    outside_bounds.id = "outside_bounds".into();
    outside_bounds.kind = ModelOptionKind::Integer {
        min: Some(0),
        max: Some(100),
    };
    definitions.extend([changed, child, leaf, changed_type, outside_bounds]);
    let kept = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice("custom".into())),
        ("toggle".into(), ModelOptionValue::Boolean(false)),
        ("budget".into(), ModelOptionValue::Integer(0)),
    ]);
    let mut values = kept.clone();
    values.extend([
        ("changed".into(), ModelOptionValue::Choice("old".into())),
        ("child".into(), ModelOptionValue::Choice("on".into())),
        ("leaf".into(), ModelOptionValue::Choice("deep".into())),
        ("changed_type".into(), ModelOptionValue::Boolean(true)),
        ("outside_bounds".into(), ModelOptionValue::Integer(101)),
        (
            "removed".into(),
            ModelOptionValue::Choice("anything".into()),
        ),
    ]);
    reconcile_model_option_values(&definitions, &mut values);
    assert_eq!(values, kept);
    assert!(validate_model_option_values(&definitions, &values, false).is_ok());
    assert!(validate_model_options(&definitions, &values).is_err());
    assert!(
        crate::parse_model_option_assignments(
            &definitions,
            &["changed=old".into()],
            values.clone()
        )
        .is_err()
    );
    values.insert("changed".into(), ModelOptionValue::Choice("new".into()));
    reconcile_model_option_values(&definitions, &mut values);
    assert_eq!(
        values.get("changed"),
        Some(&ModelOptionValue::Choice("new".into()))
    );
}
