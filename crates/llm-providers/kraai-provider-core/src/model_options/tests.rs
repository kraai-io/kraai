use super::*;
use kraai_types::ModelOptionChoice;
use serde_json::json;

#[test]
fn effort_encoding_preserves_arbitrary_values_across_apis() -> Result<()> {
    for (protocol, path) in [
        (ModelOptionsProtocol::OpenAiResponses, "/reasoning/effort"),
        (
            ModelOptionsProtocol::OpenAiChatCompletions,
            "/reasoning_effort",
        ),
        (
            ModelOptionsProtocol::AnthropicMessages,
            "/output_config/effort",
        ),
        (
            ModelOptionsProtocol::OpenRouterChatCompletions,
            "/reasoning/effort",
        ),
    ] {
        let definition = reasoning_effort_option(protocol, ["future-level".into()]);
        let values = ModelOptionValues::from([(
            "reasoning_effort".into(),
            ModelOptionValue::Choice("future-level".into()),
        )]);
        let mut body = json!({"model": "test", "reasoning": {"context": "all_turns"}});
        let headers = apply_model_options(&[definition], &values, &mut body)?;
        ensure!(body.pointer(path) == Some(&json!("future-level")));
        ensure!(body.pointer("/reasoning/context") == Some(&json!("all_turns")));
        ensure!(headers.is_empty());
    }
    Ok(())
}

#[test]
fn processing_tiers_require_explicit_override_without_forcing_a_tier() -> Result<()> {
    ensure!(service_tier_options(Vec::new()).is_empty());
    let definitions = service_tier_options(vec![ModelOptionChoice {
        id: "advertised-tier".into(),
        label: "Advertised speed".into(),
        patch: ModelRequestPatch::default(),
    }]);
    let off = ModelOptionValues::from([(
        "processing_mode_enabled".into(),
        ModelOptionValue::Boolean(false),
    )]);
    let mut body = json!({"model":"custom"});
    apply_model_options(&definitions, &off, &mut body)?;
    ensure!(body == json!({"model":"custom"}));
    let mut selected = ModelOptionValues::from([(
        "processing_mode_enabled".into(),
        ModelOptionValue::Boolean(true),
    )]);
    ensure!(apply_model_options(&definitions, &selected, &mut body).is_err());
    selected.insert(
        "service_tier".into(),
        ModelOptionValue::Choice("advertised-tier".into()),
    );
    apply_model_options(&definitions, &selected, &mut body)?;
    ensure!(body.get("service_tier") == Some(&json!("advertised-tier")));
    Ok(())
}

#[test]
fn supplied_mode_patches_compose_with_reasoning_and_headers() -> Result<()> {
    let reasoning = reasoning_effort_option(ModelOptionsProtocol::OpenAiResponses, ["deep".into()]);
    let mode = ModelOptionDefinition {
        id: "custom_speed".into(),
        label: "Speed".into(),
        description: None,
        required: true,
        active_when: None,
        binding: None,
        kind: ModelOptionKind::Choice {
            choices: vec![ModelOptionChoice {
                id: "quick".into(),
                label: "Quick".into(),
                patch: ModelRequestPatch {
                    body: BTreeMap::from([
                        ("service_tier".into(), json!("priority")),
                        ("reasoning".into(), json!({"mode": "pro"})),
                    ]),
                    headers: BTreeMap::from([("X-Feature".into(), "preview".into())]),
                },
            }],
        },
    };
    let values = ModelOptionValues::from([
        (
            "reasoning_effort".into(),
            ModelOptionValue::Choice("deep".into()),
        ),
        (
            "custom_speed".into(),
            ModelOptionValue::Choice("quick".into()),
        ),
    ]);
    let mut body = json!({"model":"custom"});
    let headers = apply_model_options(&[reasoning, mode], &values, &mut body)?;
    ensure!(
        body == json!({"model":"custom", "service_tier":"priority", "reasoning":{"effort":"deep", "mode":"pro"}})
    );
    ensure!(headers.get("x-feature").map(String::as_str) == Some("preview"));
    Ok(())
}

#[test]
fn conflicting_settings_fail_without_modifying_the_request() -> Result<()> {
    let definition =
        reasoning_effort_option(ModelOptionsProtocol::OpenAiResponses, ["deep".into()]);
    let mut conflict = definition.clone();
    conflict.id = "other".into();
    let values = ModelOptionValues::from([
        (
            "reasoning_effort".into(),
            ModelOptionValue::Choice("deep".into()),
        ),
        ("other".into(), ModelOptionValue::Choice("shallow".into())),
    ]);
    conflict.kind = ModelOptionKind::Choice {
        choices: vec![ModelOptionChoice {
            id: "shallow".into(),
            label: "Shallow".into(),
            patch: Default::default(),
        }],
    };
    let original = json!({"model":"custom", "reasoning":{"context":"all_turns"}});
    let mut body = original.clone();
    let definitions = vec![definition, conflict];
    ensure!(validate_model_option_effects(&definitions, &values).is_err());
    ensure!(apply_model_options(&definitions, &values, &mut body).is_err());
    ensure!(body == original);
    Ok(())
}

#[test]
fn integer_bindings_handle_custom_keys_and_existing_arrays() -> Result<()> {
    let definition = ModelOptionDefinition {
        id: "custom".into(),
        label: "Custom".into(),
        description: None,
        required: true,
        active_when: None,
        binding: Some(ModelOptionBinding::Body {
            path: "/extensions/0/a~1b~0c".into(),
        }),
        kind: ModelOptionKind::Integer {
            min: Some(1),
            max: Some(10),
        },
    };
    let values = ModelOptionValues::from([("custom".into(), ModelOptionValue::Integer(7))]);
    let mut body = json!({"extensions":[{}]});
    apply_model_options(std::slice::from_ref(&definition), &values, &mut body)?;
    ensure!(body == json!({"extensions":[{"a/b~c":7}]}));
    for index in ["00", "+0"] {
        let mut alias = definition.clone();
        alias.id = "alias".into();
        alias.binding = Some(ModelOptionBinding::Body {
            path: format!("/extensions/{index}/a~1b~0c"),
        });
        let mut selected = values.clone();
        selected.insert("alias".into(), ModelOptionValue::Integer(8));
        let original = body.clone();
        let definitions = vec![definition.clone(), alias];
        ensure!(apply_model_options(&definitions, &selected, &mut body).is_err());
        ensure!(body == original);
    }
    Ok(())
}

#[test]
fn unsupported_api_controls_are_not_invented() {
    assert!(reasoning_toggle_option(ModelOptionsProtocol::OpenAiResponses).is_none());
    assert!(
        reasoning_budget_option(ModelOptionsProtocol::OpenAiChatCompletions, Some(1), None)
            .is_none()
    );
}
