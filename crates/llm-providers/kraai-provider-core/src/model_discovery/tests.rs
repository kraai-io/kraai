#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert metadata-defined request effects"
)]

use color_eyre::Result;
use kraai_types::{ModelOptionValue, ModelOptionValues};
use serde_json::json;

use super::*;

#[test]
fn concrete_efforts_come_from_metadata_and_omitted_default_markers_are_removed() -> Result<()> {
    let metadata: DiscoveredModelOptions = serde_json::from_value(json!({
        "reasoning_options":[{"type":"effort","values":[null,"future-effort"]}]
    }))?;
    let definitions = metadata.definitions(ModelOptionsProtocol::OpenAiChatCompletions);
    let mut body = json!({});
    crate::apply_model_options(
        &definitions,
        &ModelOptionValues::from([(
            "reasoning_effort".into(),
            ModelOptionValue::Choice("future-effort".into()),
        )]),
        &mut body,
    )?;
    assert_eq!(body, json!({"reasoning_effort":"future-effort"}));
    assert!(
        crate::apply_model_options(&definitions, &ModelOptionValues::new(), &mut body).is_err()
    );
    Ok(())
}

#[test]
fn native_reasoning_levels_preserve_explicit_option_descriptors() -> Result<()> {
    let metadata: DiscoveredModelOptions = serde_json::from_value(json!({
        "options":[
            {"id":"reasoning_enabled","label":"Thinking","type":"boolean","required":true,
                "binding":{"type":"body","path":"/thinking/enabled"}},
            {"id":"custom_gate","label":"Custom reasoning","type":"boolean","required":true},
            {"id":"reasoning_effort","label":"Endpoint effort","type":"choice","required":true,
                "binding":{"type":"body","path":"/custom/effort"},
                "active_when":{"option":"custom_gate","value":true},
                "choices":[{"id":"endpoint-effort","label":"Endpoint effort"}]}
        ]
    }))?;
    let definitions = metadata.definitions_with_reasoning_levels(
        ModelOptionsProtocol::OpenAiChatCompletions,
        ["native-effort".into()],
    );
    assert_eq!(
        definitions
            .iter()
            .find(|option| option.id == "reasoning_effort"),
        metadata
            .options
            .iter()
            .find(|option| option.id == "reasoning_effort")
    );
    let mut body = json!({});
    crate::apply_model_options(
        &definitions,
        &ModelOptionValues::from([
            ("reasoning_enabled".into(), ModelOptionValue::Boolean(false)),
            ("custom_gate".into(), ModelOptionValue::Boolean(true)),
            (
                "reasoning_effort".into(),
                ModelOptionValue::Choice("endpoint-effort".into()),
            ),
        ]),
        &mut body,
    )?;
    assert_eq!(
        body,
        json!({"thinking":{"enabled":false},"custom":{"effort":"endpoint-effort"}})
    );
    Ok(())
}

#[test]
fn explicit_descriptors_from_each_source_preserve_conditions_over_native_shorthand() -> Result<()> {
    let catalog: DiscoveredModelOptions = serde_json::from_value(json!({
        "reasoning_options":[
            {"type":"toggle"},
            {"type":"effort","values":["catalog-effort"]},
            {"type":"budget_tokens","min":1024,"max":16384}
        ]
    }))?;
    let explicit: DiscoveredModelOptions = serde_json::from_value(json!({
        "options":[
            {"id":"custom_gate","label":"Custom gate","type":"boolean","required":true},
            {"id":"reasoning_effort","label":"Custom effort","type":"choice","required":true,
                "binding":{"type":"body","path":"/custom/effort"},
                "choices":[{"id":"custom-effort","label":"Custom effort"}]},
            {"id":"reasoning_budget","label":"Custom budget","type":"integer","required":true,
                "min":1,"max":10000,"binding":{"type":"body","path":"/custom/budget"},
                "active_when":{"option":"custom_gate","value":true}}
        ]
    }))?;
    for catalog_explicit in [false, true] {
        let mut catalog = catalog.clone();
        let mut native = DiscoveredModelOptions::default();
        if catalog_explicit {
            catalog.options.clone_from(&explicit.options);
        } else {
            native.options.clone_from(&explicit.options);
        }
        let definitions = native.definitions_with_fallback(
            ModelOptionsProtocol::OpenRouterChatCompletions,
            ["native-effort".into()],
            &catalog,
        );
        for expected in &explicit.options {
            assert_eq!(
                definitions.iter().find(|option| option.id == expected.id),
                Some(expected)
            );
        }
        let mut body = json!({});
        crate::apply_model_options(
            &definitions,
            &ModelOptionValues::from([
                ("reasoning_enabled".into(), ModelOptionValue::Boolean(false)),
                ("custom_gate".into(), ModelOptionValue::Boolean(true)),
                (
                    "reasoning_effort".into(),
                    ModelOptionValue::Choice("custom-effort".into()),
                ),
                ("reasoning_budget".into(), ModelOptionValue::Integer(4096)),
            ]),
            &mut body,
        )?;
        assert_eq!(
            body,
            json!({"reasoning":{"enabled":false},"custom":{"effort":"custom-effort","budget":4096}})
        );
    }
    Ok(())
}

#[test]
fn independent_modes_allow_explicit_off_and_compose_body_and_header_effects() -> Result<()> {
    let metadata: DiscoveredModelOptions = serde_json::from_value(json!({
        "experimental":{"modes":{
            "fast":{"provider":{"body":{"speed":"fast"},"headers":{"x-speed":"fast"}}},
            "pro":{"provider":{"body":{"quality":"pro"}}}
        }}
    }))?;
    let definitions = metadata.definitions(ModelOptionsProtocol::OpenAiChatCompletions);
    for enabled in [false, true] {
        let mut body = json!({"model":"custom"});
        let headers = crate::apply_model_options(
            &definitions,
            &ModelOptionValues::from([
                ("mode:fast".into(), ModelOptionValue::Boolean(enabled)),
                ("mode:pro".into(), ModelOptionValue::Boolean(true)),
            ]),
            &mut body,
        )?;
        assert_eq!(body.get("speed"), enabled.then_some(&json!("fast")));
        assert_eq!(body.get("quality"), Some(&json!("pro")));
        assert_eq!(
            headers.get("x-speed").map(String::as_str),
            enabled.then_some("fast")
        );
    }
    Ok(())
}

#[test]
fn unsupported_protocol_controls_are_not_exposed_and_toggle_gates_dependent_controls() -> Result<()>
{
    let metadata: DiscoveredModelOptions = serde_json::from_value(json!({
        "reasoning_options":[{"type":"toggle"},{"type":"budget_tokens","min":1024,"max":16384}]
    }))?;
    assert!(
        metadata
            .definitions(ModelOptionsProtocol::OpenAiChatCompletions)
            .is_empty()
    );
    let definitions = metadata.definitions(ModelOptionsProtocol::OpenRouterChatCompletions);
    let mut body = json!({});
    crate::apply_model_options(
        &definitions,
        &ModelOptionValues::from([("reasoning_enabled".into(), ModelOptionValue::Boolean(false))]),
        &mut body,
    )?;
    assert_eq!(body, json!({"reasoning":{"enabled":false}}));
    let options = ModelOptionValues::from([
        ("reasoning_enabled".into(), ModelOptionValue::Boolean(true)),
        ("reasoning_budget".into(), ModelOptionValue::Integer(4096)),
    ]);
    crate::apply_model_options(&definitions, &options, &mut body)?;
    assert_eq!(
        body,
        json!({"reasoning":{"enabled":true,"max_tokens":4096}})
    );
    Ok(())
}
