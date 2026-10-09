#![expect(
    clippy::panic_in_result_fn,
    reason = "model option tests assert after fallible fixture parsing"
)]

use super::*;
use crate::app::{KeyCode, KeyEvent, KeyModifiers};
use kraai_types::{ModelOptionDefinition, ModelOptionKind, ModelOptionValue, ModelOptionValues};

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

pub(super) fn configure_optional(harness: &mut TestHarness) -> color_eyre::Result<()> {
    configure(harness)?;
    harness.app.startup_options.ci = true;
    let model = harness
        .app
        .state
        .models_by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    for option in &mut model.options {
        option.required = false;
    }
    harness.app.state.selected_model_options = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice("high".into())),
        ("thinking".into(), ModelOptionValue::Boolean(true)),
        ("budget".into(), ModelOptionValue::Integer(50)),
    ]);
    Ok(())
}

fn key(harness: &mut TestHarness, code: KeyCode) {
    harness
        .app
        .handle_model_options_key_event(KeyEvent::new(code, KeyModifiers::NONE));
}

fn edit(harness: &mut TestHarness, id: &str) -> color_eyre::Result<()> {
    harness.app.open_model_options();
    harness.app.state.option_menu_index = harness
        .app
        .state
        .active_model_options()
        .iter()
        .position(|option| option.id == id)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing active option {id}"))?;
    key(harness, KeyCode::Enter);
    Ok(())
}

#[test]
fn optional_choice_and_boolean_pickers_show_and_apply_unset() -> color_eyre::Result<()> {
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    for id in ["effort", "thinking"] {
        let mut harness = test_harness();
        configure_optional(&mut harness)?;
        edit(&mut harness, id)?;
        assert_eq!(harness.app.state.option_choice_index, 2);
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        (&harness.app.state).render(area, &mut buffer);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Unset"));
        key(&mut harness, KeyCode::Up);
        key(&mut harness, KeyCode::Up);
        key(&mut harness, KeyCode::Enter);
        assert!(!harness.app.state.selected_model_options.contains_key(id));
        assert!(!harness.app.state.option_editing);
        assert_eq!(harness.app.state.status, format!("Unset {id}"));
        if id == "thinking" {
            assert!(
                !harness
                    .app
                    .state
                    .selected_model_options
                    .contains_key("budget")
            );
        }
    }
    Ok(())
}

#[test]
fn optional_integer_editor_clears_empty_input() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    edit(&mut harness, "budget")?;
    assert_eq!(harness.app.state.option_editor_input, "50");
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Enter);
    assert!(
        !harness
            .app
            .state
            .selected_model_options
            .contains_key("budget")
    );
    assert!(!harness.app.state.option_editing);
    Ok(())
}

#[test]
fn option_command_clears_all_optional_types() -> color_eyre::Result<()> {
    for id in ["effort", "thinking", "budget"] {
        let mut harness = test_harness();
        configure_optional(&mut harness)?;
        harness.app.handle_option_command(vec![id, "--clear"]);
        assert!(!harness.app.state.selected_model_options.contains_key(id));
        assert_eq!(harness.app.state.status, format!("Unset {id}"));
    }
    Ok(())
}

#[test]
fn required_controls_reject_clear_and_keep_concrete_picker_choices() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    let model = harness
        .app
        .state
        .models_by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    for option in &mut model.options {
        option.required = true;
    }
    let values = harness.app.state.selected_model_options.clone();
    for id in ["effort", "thinking", "budget"] {
        harness.app.handle_option_command(vec![id, "--clear"]);
        assert!(harness.app.state.status.contains("cannot be cleared"));
        assert_eq!(harness.app.state.selected_model_options, values);
    }
    for id in ["effort", "thinking"] {
        edit(&mut harness, id)?;
        assert_eq!(harness.app.state.option_choice_index, 1);
        key(&mut harness, KeyCode::Up);
        key(&mut harness, KeyCode::Enter);
        assert!(harness.app.state.selected_model_options.contains_key(id));
    }
    harness.app.state.selected_model_options = values.clone();
    edit(&mut harness, "budget")?;
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editing);
    assert!(harness.app.state.status.contains("cannot be cleared"));
    assert_eq!(harness.app.state.selected_model_options, values);
    Ok(())
}

#[test]
fn optional_false_and_zero_remain_distinct_from_unset() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    harness
        .app
        .set_model_option("thinking", "false")
        .map_err(color_eyre::eyre::Error::msg)?;
    edit(&mut harness, "thinking")?;
    assert_eq!(harness.app.state.option_choice_index, 1);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.selected_model_options.get("thinking"),
        Some(&ModelOptionValue::Boolean(false))
    );
    harness
        .app
        .set_model_option("thinking", "true")
        .map_err(color_eyre::eyre::Error::msg)?;
    let model = harness
        .app
        .state
        .models_by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    if let Some(option) = model
        .options
        .iter_mut()
        .find(|option| option.id == "budget")
    {
        option.kind = ModelOptionKind::Integer {
            min: Some(0),
            max: Some(100),
        };
    }
    harness
        .app
        .set_model_option("budget", "0")
        .map_err(color_eyre::eyre::Error::msg)?;
    edit(&mut harness, "budget")?;
    assert_eq!(harness.app.state.option_editor_input, "0");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.selected_model_options.get("budget"),
        Some(&ModelOptionValue::Integer(0))
    );
    Ok(())
}

#[test]
fn picker_can_select_a_literal_clear_choice() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    let model = harness
        .app
        .state
        .models_by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    let option = model
        .options
        .iter_mut()
        .find(|option| option.id == "effort")
        .ok_or_else(|| color_eyre::eyre::eyre!("missing effort option"))?;
    if let ModelOptionKind::Choice { choices } = &mut option.kind {
        choices.push(kraai_types::ModelOptionChoice {
            id: "--clear".into(),
            label: "Literal clear".into(),
            patch: kraai_types::ModelRequestPatch::default(),
        });
    }
    edit(&mut harness, "effort")?;
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.selected_model_options.get("effort"),
        Some(&ModelOptionValue::Choice("--clear".into()))
    );
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
