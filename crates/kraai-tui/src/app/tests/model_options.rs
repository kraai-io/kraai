#![expect(
    clippy::panic_in_result_fn,
    reason = "model option tests assert after fallible fixture parsing"
)]

use super::*;
use crate::app::model_options::active_options;
use crate::app::{KeyCode, KeyEvent, KeyModifiers, UiMode};
use kraai_types::{
    ModelOptionDefinition, ModelOptionKind, ModelOptionValue, ModelOptionValues, SessionId,
};

mod errors;
mod refresh;

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
        .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
}

fn ctrl(harness: &mut TestHarness, ch: char) {
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
}

fn type_text(harness: &mut TestHarness, text: &str) {
    for ch in text.chars() {
        key(harness, KeyCode::Char(ch));
    }
}

fn screen(state: &AppState, width: u16, height: u16) -> String {
    use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    state.render(area, &mut buffer);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn active_ids(harness: &TestHarness) -> Vec<String> {
    harness
        .app
        .state
        .selected_model()
        .into_iter()
        .flat_map(|model| active_options(model, &harness.app.state.selected_model_options))
        .map(|option| option.id.clone())
        .collect()
}

fn model_mut(harness: &mut TestHarness) -> color_eyre::Result<&mut kraai_runtime::Model> {
    harness
        .app
        .state
        .models_by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))
}

/// Opens the settings of the model in use through the model list and highlights `id`.
fn open_settings(harness: &mut TestHarness, id: &str) -> color_eyre::Result<()> {
    harness.app.handle_command("model");
    ctrl(harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    harness.app.state.sheet_index = active_ids(harness)
        .iter()
        .position(|option| option == id)
        .ok_or_else(|| color_eyre::eyre::eyre!("missing active option {id}"))?;
    Ok(())
}

/// Sets an option on the model in use.
pub(super) fn set(harness: &mut TestHarness, id: &str, input: &str) -> Result<(), String> {
    let provider = harness.app.state.selected_provider_id.clone();
    let model = harness.app.state.selected_model_id.clone();
    harness.app.set_model_option(
        provider.as_deref().unwrap_or_default(),
        model.as_deref().unwrap_or_default(),
        id,
        input,
    )
}

fn value<'a>(harness: &'a TestHarness, id: &str) -> Option<&'a ModelOptionValue> {
    harness.app.state.selected_model_options.get(id)
}

#[test]
fn arrows_step_through_unset_and_every_choice_and_apply_at_once() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "effort")?;
    assert!(screen(&harness.app.state, 100, 30).contains("Effort    unset  Low  High"));
    key(&mut harness, KeyCode::Right);
    assert_eq!(value(&harness, "effort"), None);
    assert_eq!(harness.app.state.status, "Unset effort");
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    key(&mut harness, KeyCode::Left);
    key(&mut harness, KeyCode::Left);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Left);
    assert_eq!(
        value(&harness, "thinking"),
        Some(&ModelOptionValue::Boolean(false))
    );
    assert_eq!(value(&harness, "budget"), None);
    assert_eq!(active_ids(&harness), ["effort", "thinking"]);
    key(&mut harness, KeyCode::Left);
    assert_eq!(value(&harness, "thinking"), None);
    assert_eq!(harness.app.state.sheet_index, 1);
    Ok(())
}

#[test]
fn delete_unsets_the_highlighted_setting() -> color_eyre::Result<()> {
    for id in ["effort", "thinking", "budget"] {
        let mut harness = test_harness();
        configure_optional(&mut harness)?;
        open_settings(&mut harness, id)?;
        key(&mut harness, KeyCode::Delete);
        assert_eq!(value(&harness, id), None);
        assert_eq!(harness.app.state.status, format!("Unset {id}"));
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
    Ok(())
}

#[test]
fn typed_text_sets_the_highlighted_setting() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    for (id, typed, expected) in [
        ("budget", "75", ModelOptionValue::Integer(75)),
        ("effort", "LOW", ModelOptionValue::Choice("low".into())),
        ("thinking", "off", ModelOptionValue::Boolean(false)),
    ] {
        open_settings(&mut harness, id)?;
        type_text(&mut harness, typed);
        assert!(screen(&harness.app.state, 100, 30).contains(&format!("/model › Model › {typed}")));
        key(&mut harness, KeyCode::Enter);
        assert_eq!(value(&harness, id), Some(&expected));
        assert!(harness.app.state.sheet_query.is_empty());
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    Ok(())
}

#[test]
fn required_settings_reject_unset_and_never_offer_it() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    for option in &mut model_mut(&mut harness)?.options {
        option.required = true;
    }
    let values = harness.app.state.selected_model_options.clone();
    for id in ["effort", "thinking", "budget"] {
        open_settings(&mut harness, id)?;
        key(&mut harness, KeyCode::Delete);
        assert!(
            harness
                .app
                .state
                .sheet_notice
                .as_ref()
                .is_some_and(|notice| notice.contains("cannot be cleared")),
            "{:?}",
            harness.app.state.sheet_notice
        );
        assert!(screen(&harness.app.state, 100, 30).contains("cannot be cleared"));
        assert_eq!(harness.app.state.selected_model_options, values);
    }
    for id in ["effort", "thinking"] {
        open_settings(&mut harness, id)?;
        key(&mut harness, KeyCode::Right);
        assert!(value(&harness, id).is_some());
        key(&mut harness, KeyCode::Right);
        assert_eq!(value(&harness, id), values.get(id));
    }
    Ok(())
}

#[test]
fn off_and_zero_remain_distinct_from_unset() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    if let Some(option) = model_mut(&mut harness)?
        .options
        .iter_mut()
        .find(|option| option.id == "budget")
    {
        option.kind = ModelOptionKind::Integer {
            min: Some(0),
            max: Some(100),
        };
    }
    open_settings(&mut harness, "budget")?;
    type_text(&mut harness, "0");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "budget"),
        Some(&ModelOptionValue::Integer(0))
    );
    key(&mut harness, KeyCode::Enter);
    assert!(screen(&harness.app.state, 100, 30).contains("Model (High, thinking, budget 0)"));

    open_settings(&mut harness, "thinking")?;
    type_text(&mut harness, "off");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "thinking"),
        Some(&ModelOptionValue::Boolean(false))
    );
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        value(&harness, "thinking"),
        Some(&ModelOptionValue::Boolean(true))
    );
    Ok(())
}

#[test]
fn a_choice_named_like_a_flag_or_with_spaces_can_be_selected() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    let option = model_mut(&mut harness)?
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
    open_settings(&mut harness, "effort")?;
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("--clear".into()))
    );
    key(&mut harness, KeyCode::Left);
    type_text(&mut harness, "literal clear");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("--clear".into()))
    );
    Ok(())
}

#[test]
fn backspace_and_escape_step_back_through_the_model_list() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "effort")?;
    assert!(screen(&harness.app.state, 100, 30).contains("/model › Model › "));
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    ctrl(&mut harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.input.is_empty());

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(harness.app.state.input, "/model");
    Ok(())
}

#[test]
fn enter_uses_the_model_and_ctrl_s_opens_its_settings() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    let mut plain = model_mut(&mut harness)?.clone();
    plain.id = "plain".into();
    plain.name = "Plain".into();
    plain.options.clear();
    harness
        .app
        .state
        .models_by_provider
        .entry("provider".into())
        .or_default()
        .push(plain);

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Tab);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(rendered.contains("tab or ↑↓ select · enter use · esc close"));
    ctrl(&mut harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::Model);
    assert!(screen(&harness.app.state, 100, 30).contains("Plain has no other settings"));
    key(&mut harness, KeyCode::Right);
    assert!(harness.app.state.sheet_notice.is_none());
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("plain")
    );

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Tab);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(rendered.contains("tab or ↑↓ select · ctrl+s settings · enter use · esc close"));
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("model")
    );
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    Ok(())
}

/// Adds a second model whose only setting is a required reasoning effort.
fn add_effort_model(harness: &mut TestHarness, extra: bool) -> color_eyre::Result<()> {
    let mut options: Vec<ModelOptionDefinition> = serde_json::from_value(serde_json::json!([
        {"id":"reasoning_effort","label":"Reasoning effort","type":"choice","required":true,"binding":{"type":"body","path":"/effort"},"choices":[{"id":"low","label":"low"},{"id":"medium","label":"medium"},{"id":"high","label":"high"}]},
        {"id":"thinking","label":"Thinking","type":"boolean","required":false}
    ]))?;
    if !extra {
        options.truncate(1);
    }
    harness
        .app
        .state
        .models_by_provider
        .entry("provider".into())
        .or_default()
        .push(kraai_runtime::Model {
            id: "reasoner".into(),
            name: "Reasoner".into(),
            max_context: None,
            options,
        });
    Ok(())
}

fn remembered<'a>(harness: &'a TestHarness, model: &str, id: &str) -> Option<&'a ModelOptionValue> {
    harness
        .app
        .state
        .remembered_model_options
        .get("provider")?
        .get(model)?
        .get(id)
}

#[test]
fn arrows_change_reasoning_effort_on_the_highlighted_model_only() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    add_effort_model(&mut harness, false)?;
    let before = harness.app.state.selected_model_options.clone();

    harness.app.handle_command("model");
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(!rendered.contains("low  medium  high"));
    assert!(!rendered.contains("←→ effort"));
    key(&mut harness, KeyCode::Left);
    assert_eq!(harness.app.state.selected_model_options, before);

    key(&mut harness, KeyCode::Down);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(rendered.contains("Reasoner  low  medium  high"));
    assert!(rendered.contains("tab or ↑↓ select · ←→ effort · enter use · esc close"));
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        remembered(&harness, "reasoner", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    key(&mut harness, KeyCode::Left);
    assert_eq!(
        remembered(&harness, "reasoner", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    assert_eq!(harness.app.state.mode, UiMode::Model);
    assert_eq!(harness.app.state.sheet_index, 1);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("model")
    );
    assert_eq!(harness.app.state.selected_model_options, before);

    key(&mut harness, KeyCode::Up);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(!rendered.contains("low  medium  high"));
    assert!(rendered.contains("Reasoner  high"));
    assert_eq!(
        remembered(&harness, "reasoner", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("reasoner")
    );
    assert_eq!(
        value(&harness, "reasoning_effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        value(&harness, "reasoning_effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    ctrl(&mut harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::Model);
    assert_eq!(
        harness.app.state.sheet_notice.as_deref(),
        Some("Reasoner has no other settings")
    );
    Ok(())
}

#[test]
fn highlighted_model_keeps_its_other_settings_beside_the_effort_choices() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    add_effort_model(&mut harness, true)?;
    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Down);
    assert!(
        screen(&harness.app.state, 100, 30)
            .contains("tab or ↑↓ select · ←→ effort · ctrl+s settings · enter use · esc close")
    );
    ctrl(&mut harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Right);
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        remembered(&harness, "reasoner", "thinking"),
        Some(&ModelOptionValue::Boolean(true))
    );
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    key(&mut harness, KeyCode::Right);
    key(&mut harness, KeyCode::Right);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(rendered.contains("Reasoner  low  medium  high  thinking"));
    key(&mut harness, KeyCode::Up);
    assert!(screen(&harness.app.state, 100, 30).contains("Reasoner  medium, thinking"));
    Ok(())
}

#[test]
fn models_never_set_up_stay_unset_whatever_the_model_in_use_holds() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    add_effort_model(&mut harness, true)?;
    let models = harness
        .app
        .state
        .models_by_provider
        .entry("provider".into())
        .or_default();
    let mut fresh = models
        .last()
        .cloned()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    fresh.id = "fresh".into();
    fresh.name = "Fresh".into();
    models.push(fresh);
    let row = |harness: &TestHarness, name: &str| {
        let text: Vec<char> = screen(&harness.app.state, 100, 30).chars().collect();
        text.chunks(100)
            .map(|row| row.iter().collect::<String>())
            .find(|row| row.contains(name))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };

    harness.app.handle_command("model");
    assert_eq!(row(&harness, "Reasoner"), "Reasoner");
    assert_eq!(row(&harness, "Fresh"), "Fresh");
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("reasoner")
    );
    assert!(harness.app.state.selected_model_options.is_empty());

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Right);
    ctrl(&mut harness, 's');
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Right);
    key(&mut harness, KeyCode::Right);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(
        harness.app.state.selected_model_options,
        ModelOptionValues::from([
            (
                "reasoning_effort".into(),
                ModelOptionValue::Choice("low".into())
            ),
            ("thinking".into(), ModelOptionValue::Boolean(true)),
        ])
    );
    assert!(
        harness
            .app
            .state
            .model_option_values("provider", "fresh")
            .is_empty()
    );
    assert_eq!(row(&harness, "Fresh"), "Fresh");
    assert!(row(&harness, "Model").contains("High, thinking, budget 50"));

    key(&mut harness, KeyCode::Down);
    assert_eq!(row(&harness, "Fresh"), "▌ Fresh     low  medium  high");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("fresh")
    );
    assert!(harness.app.state.selected_model_options.is_empty());
    assert_eq!(
        remembered(&harness, "reasoner", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    Ok(())
}

#[test]
fn tab_and_shift_tab_walk_the_model_list_and_the_settings() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    let mut second = model_mut(&mut harness)?.clone();
    second.id = "second".into();
    harness
        .app
        .state
        .models_by_provider
        .entry("provider".into())
        .or_default()
        .push(second);
    harness.app.handle_command("model");
    for (code, expected) in [
        (KeyCode::Tab, 1),
        (KeyCode::Tab, 0),
        (KeyCode::BackTab, 1),
        (KeyCode::BackTab, 0),
    ] {
        key(&mut harness, code);
        assert_eq!(harness.app.state.sheet_index, expected);
        assert_eq!(harness.app.state.mode, UiMode::Model);
    }
    ctrl(&mut harness, 's');
    for (code, expected) in [
        (KeyCode::Tab, 1),
        (KeyCode::BackTab, 0),
        (KeyCode::BackTab, 2),
    ] {
        key(&mut harness, code);
        assert_eq!(harness.app.state.sheet_index, expected);
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
    Ok(())
}

#[test]
fn each_model_remembers_its_own_settings() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    let mut second = model_mut(&mut harness)?.clone();
    second.id = "second".into();
    second.name = "Second".into();
    harness
        .app
        .state
        .models_by_provider
        .entry("provider".into())
        .or_default()
        .push(second);

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Down);
    ctrl(&mut harness, 's');
    type_text(&mut harness, "low");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("model")
    );
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("second")
    );
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("model")
    );
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
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
    assert!(set(&mut harness, "effort", "high").is_ok());
    assert!(set(&mut harness, "thinking", "true").is_ok());
    assert!(set(&mut harness, "budget", "101").is_err());
    assert!(set(&mut harness, "budget", "50").is_ok());
    assert_eq!(
        harness.app.state.selected_model_options.get("budget"),
        Some(&ModelOptionValue::Integer(50))
    );
    assert!(set(&mut harness, "thinking", "false").is_ok());
    assert!(
        !harness
            .app
            .state
            .selected_model_options
            .contains_key("budget")
    );
    assert!(set(&mut harness, "budget", "50").is_err());
    assert_eq!(active_ids(&harness), ["effort", "thinking"]);
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
            result: Ok(SessionId::new("session")),
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
        .reset_chat_session(Some(SessionId::new("session")), "loaded");
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
            session_id: SessionId::new("session"),
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
            session_id: SessionId::new("session"),
            result: Box::new(Ok(snapshot)),
        });
    assert_eq!(
        harness.app.state.selected_model_options.get("effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    Ok(())
}
