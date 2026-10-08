#![expect(
    clippy::panic_in_result_fn,
    reason = "model option tests assert after fallible fixture parsing"
)]

use super::*;
use kraai_types::{ModelOptionDefinition, ModelOptionValue};

pub(super) fn configure(harness: &mut TestHarness) -> color_eyre::Result<()> {
    let options: Vec<ModelOptionDefinition> = serde_json::from_value(serde_json::json!([
        {"id":"effort","label":"Effort","type":"choice","required":true,"choices":[{"id":"low","label":"Low"},{"id":"high","label":"High"}]},
        {"id":"thinking","label":"Thinking","type":"boolean","required":true},
        {"id":"budget","label":"Budget","type":"integer","min":10,"max":100,"required":true,"binding":{"type":"body","path":"/budget"},"active_when":{"option":"thinking","value":true}}
    ]))?;
    harness.app.state.models_by_provider.insert(
        "provider".into(),
        vec![kraai_runtime::Model {
            id: "model".into(),
            name: "Model".into(),
            max_context: None,
            options,
        }],
    );
    harness.app.state.config_loaded = true;
    harness.app.state.selected_provider_id = Some("provider".into());
    harness.app.state.selected_model_id = Some("model".into());
    harness.app.state.selected_profile_id = Some("coding".into());
    Ok(())
}

#[test]
fn missing_options_block_submission_without_selecting_defaults() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.submit_message("Hello".into());
    assert!(harness.drain_requests().is_empty());
    assert!(harness.app.state.selected_model_options.is_empty());
    assert!(harness.app.state.status.contains("effort"));
    assert!(harness.app.state.status.contains("thinking"));
    Ok(())
}

#[test]
fn typed_options_validate_and_hide_inactive_budget() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.startup_options.ci = true;
    assert!(harness.app.set_model_option("effort", "high").is_ok());
    assert!(harness.app.set_model_option("thinking", "true").is_ok());
    assert!(harness.app.set_model_option("budget", "101").is_err());
    assert!(harness.app.set_model_option("budget", "50").is_ok());
    assert_eq!(
        harness.app.state.selected_model_options.get("budget"),
        Some(&ModelOptionValue::Integer(50))
    );
    assert!(harness.app.set_model_option("thinking", "false").is_ok());
    assert!(
        !harness
            .app
            .state
            .selected_model_options
            .contains_key("budget")
    );
    assert!(harness.app.set_model_option("budget", "50").is_err());
    assert_eq!(harness.app.state.active_model_options().len(), 2);
    Ok(())
}

#[test]
fn deferred_submission_retains_options_after_local_selection_changes() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.state.selected_model_options = std::collections::BTreeMap::from([
        ("effort".into(), ModelOptionValue::Choice("high".into())),
        ("thinking".into(), ModelOptionValue::Boolean(false)),
    ]);
    let selected = harness.app.state.selected_model_options.clone();
    harness.app.submit_message("Hello".into());
    let creation_id = harness
        .drain_requests()
        .into_iter()
        .find_map(|request| match request {
            RuntimeRequest::CreateSession { creation_id, .. } => Some(creation_id),
            _ => None,
        })
        .ok_or_else(|| color_eyre::eyre::eyre!("missing creation request"))?;
    harness
        .app
        .state
        .selected_model_options
        .insert("effort".into(), ModelOptionValue::Choice("low".into()));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::CreateSession {
            creation_id,
            result: Ok("session".into()),
        });
    assert!(harness.drain_requests().into_iter().any(|request| matches!(request, RuntimeRequest::SendMessage { options, .. } if options == selected)));
    Ok(())
}

#[test]
fn session_restore_does_not_overwrite_later_option_edits() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness
        .app
        .reset_chat_session(Some("session".into()), "loaded");
    let mut snapshot = session_snapshot(None);
    let selected = std::collections::BTreeMap::from([(
        "effort".into(),
        ModelOptionValue::Choice("high".into()),
    )]);
    snapshot.session.selected_model = Some(kraai_types::ModelSelection {
        provider_id: kraai_types::ProviderId::new("provider"),
        model_id: kraai_types::ModelId::new("model"),
        options: selected.clone(),
    });
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: "session".into(),
            result: Box::new(Ok(snapshot.clone())),
        });
    assert_eq!(harness.app.state.selected_model_options, selected);
    harness
        .app
        .state
        .selected_model_options
        .insert("effort".into(), ModelOptionValue::Choice("low".into()));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            model_save_id: 0,
            session_id: "session".into(),
            result: Box::new(Ok(snapshot)),
        });
    assert_eq!(
        harness.app.state.selected_model_options.get("effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    Ok(())
}

#[test]
fn startup_flags_parse_discovered_types_once() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.startup_options.options = vec![
        "effort=high".into(),
        "thinking=true".into(),
        "budget=50".into(),
    ];
    assert!(harness.app.apply_startup_model_options().is_ok());
    assert_eq!(
        harness.app.state.selected_model_options.get("thinking"),
        Some(&ModelOptionValue::Boolean(true))
    );
    assert_eq!(
        harness.app.state.selected_model_options.get("budget"),
        Some(&ModelOptionValue::Integer(50))
    );
    harness
        .app
        .state
        .selected_model_options
        .insert("effort".into(), ModelOptionValue::Choice("low".into()));
    assert!(harness.app.apply_startup_model_options().is_ok());
    assert_eq!(
        harness.app.state.selected_model_options.get("effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    Ok(())
}
