use super::*;
use crate::app::model_options::active_options;
use crate::app::types::UiMode;
use color_eyre::eyre::Result;
use kraai_types::{ModelOptionDefinition, ModelOptionKind, ModelOptionValue, ModelOptionValues};
use ratatui::crossterm::event::KeyCode;

mod errors;
mod refresh;

pub(super) fn configure(harness: &mut TestHarness) {
    let options: Vec<ModelOptionDefinition> = serde_json::from_value(serde_json::json!([
        {"id":"effort","label":"Effort","type":"choice","required":true,"choices":[{"id":"low","label":"Low"},{"id":"high","label":"High"}]},
        {"id":"thinking","label":"Thinking","type":"boolean","required":true},
        {"id":"budget","label":"Budget","type":"integer","min":10,"max":100,"required":true,"binding":{"type":"body","path":"/budget"},"active_when":{"option":"thinking","value":true}}
    ])).unwrap();
    harness.app.state.models.by_provider.insert(
        "provider".into(),
        vec![kraai_runtime::Model {
            id: "model".into(),
            name: "Model".into(),
            max_context: None,
            options,
        }],
    );
    harness.app.state.config_loaded = true;
    harness.app.state.models.selected_provider_id = Some("provider".into());
    harness.app.state.models.selected_model_id = Some("model".into());
    harness.app.state.agents.selected_profile_id = Some("coding".into());
}

pub(super) fn configure_optional(harness: &mut TestHarness) {
    configure(harness);
    let model = harness
        .app
        .state
        .models
        .by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .expect("missing model");
    for option in &mut model.options {
        option.required = false;
    }
    harness.app.state.models.selected_options = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice("high".into())),
        ("thinking".into(), ModelOptionValue::Boolean(true)),
        ("budget".into(), ModelOptionValue::Integer(50)),
    ]);
}

fn active_ids(harness: &TestHarness) -> Vec<String> {
    harness
        .app
        .state
        .selected_model()
        .into_iter()
        .flat_map(|model| active_options(model, &harness.app.state.models.selected_options))
        .map(|option| option.id.clone())
        .collect()
}

fn model_mut(harness: &mut TestHarness) -> &mut kraai_runtime::Model {
    harness
        .app
        .state
        .models
        .by_provider
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .expect("missing model")
}

fn open_settings(harness: &mut TestHarness, id: &str) {
    harness.app.handle_command("model");
    ctrl(harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    harness.app.state.sheet.index = active_ids(harness)
        .iter()
        .position(|option| option == id)
        .unwrap_or_else(|| panic!("missing active option {id}"));
}

pub(super) fn set(harness: &mut TestHarness, id: &str, input: &str) -> Result<(), String> {
    let provider = harness.app.state.models.selected_provider_id.clone();
    let model = harness.app.state.models.selected_model_id.clone();
    harness.app.set_model_option(
        provider.as_deref().unwrap_or_default(),
        model.as_deref().unwrap_or_default(),
        id,
        input,
    )
}

fn value<'a>(harness: &'a TestHarness, id: &str) -> Option<&'a ModelOptionValue> {
    harness.app.state.models.selected_options.get(id)
}

#[test]
fn arrows_step_through_unset_and_every_choice_and_apply_at_once() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    open_settings(&mut harness, "effort");
    assert!(screen(&harness.app.state, 100, 30).contains("Effort    unset  Low  High"));
    key(&mut harness, KeyCode::Right);
    assert_eq!(value(&harness, "effort"), None);
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
    assert_eq!(harness.app.state.sheet.index, 1);
}

#[test]
fn delete_unsets_the_highlighted_setting() {
    for id in ["effort", "thinking", "budget"] {
        let mut harness = test_harness();
        configure_optional(&mut harness);
        open_settings(&mut harness, id);
        key(&mut harness, KeyCode::Delete);
        assert_eq!(value(&harness, id), None);
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
}

#[test]
fn typed_text_sets_the_highlighted_setting() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    for (id, typed, expected) in [
        ("budget", "75", ModelOptionValue::Integer(75)),
        ("effort", "LOW", ModelOptionValue::Choice("low".into())),
        ("thinking", "off", ModelOptionValue::Boolean(false)),
    ] {
        open_settings(&mut harness, id);
        type_text(&mut harness, typed);
        assert!(screen(&harness.app.state, 100, 30).contains(&format!("/model › Model › {typed}")));
        key(&mut harness, KeyCode::Enter);
        assert_eq!(value(&harness, id), Some(&expected));
        assert!(harness.app.state.sheet.query.is_empty());
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
}

#[test]
fn required_settings_reject_unset_and_never_offer_it() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    for option in &mut model_mut(&mut harness).options {
        option.required = true;
    }
    let values = harness.app.state.models.selected_options.clone();
    for id in ["effort", "thinking", "budget"] {
        open_settings(&mut harness, id);
        key(&mut harness, KeyCode::Delete);
        assert!(
            harness
                .app
                .state
                .sheet
                .notice
                .as_ref()
                .is_some_and(|notice| notice.contains("cannot be cleared")),
            "{:?}",
            harness.app.state.sheet.notice
        );
        assert!(screen(&harness.app.state, 100, 30).contains("cannot be cleared"));
        assert_eq!(harness.app.state.models.selected_options, values);
    }
    for id in ["effort", "thinking"] {
        open_settings(&mut harness, id);
        key(&mut harness, KeyCode::Right);
        assert!(value(&harness, id).is_some());
        key(&mut harness, KeyCode::Right);
        assert_eq!(value(&harness, id), values.get(id));
    }
}

#[test]
fn off_and_zero_remain_distinct_from_unset() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    if let Some(option) = model_mut(&mut harness)
        .options
        .iter_mut()
        .find(|option| option.id == "budget")
    {
        option.kind = ModelOptionKind::Integer {
            min: Some(0),
            max: Some(100),
        };
    }
    open_settings(&mut harness, "budget");
    type_text(&mut harness, "0");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "budget"),
        Some(&ModelOptionValue::Integer(0))
    );
    key(&mut harness, KeyCode::Enter);
    assert!(screen(&harness.app.state, 100, 30).contains("Model  High, thinking, budget 0"));

    open_settings(&mut harness, "thinking");
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
}

#[test]
fn a_choice_named_like_a_flag_or_with_spaces_can_be_selected() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    let option = model_mut(&mut harness)
        .options
        .iter_mut()
        .find(|option| option.id == "effort")
        .expect("missing effort option");
    if let ModelOptionKind::Choice { choices } = &mut option.kind {
        choices.push(kraai_types::ModelOptionChoice {
            id: "--clear".into(),
            label: "Literal clear".into(),
            patch: kraai_types::ModelRequestPatch::default(),
        });
    }
    open_settings(&mut harness, "effort");
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
}

#[test]
fn backspace_and_escape_step_back_through_the_model_list() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    open_settings(&mut harness, "effort");
    assert_eq!(composer(&harness), "/model › Model ›");
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    ctrl(&mut harness, 's');
    assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Model);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.composer.text.is_empty());

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(harness.app.state.composer.text, "/model");
}

#[test]
fn enter_uses_the_model_and_ctrl_s_opens_its_settings() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    let mut plain = model_mut(&mut harness).clone();
    plain.id = "plain".into();
    plain.name = "Plain".into();
    plain.options.clear();
    harness
        .app
        .state
        .models
        .by_provider
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
    assert!(harness.app.state.sheet.notice.is_none());
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.models.selected_model_id.as_deref(),
        Some("plain")
    );

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Tab);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(rendered.contains("tab or ↑↓ select · ctrl+s settings · enter use · esc close"));
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.models.selected_model_id.as_deref(),
        Some("model")
    );
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
}

fn add_effort_model(harness: &mut TestHarness, extra: bool) {
    let mut options: Vec<ModelOptionDefinition> = serde_json::from_value(serde_json::json!([
        {"id":"reasoning_effort","label":"Reasoning effort","type":"choice","required":true,"binding":{"type":"body","path":"/effort"},"choices":[{"id":"low","label":"low"},{"id":"medium","label":"medium"},{"id":"high","label":"high"}]},
        {"id":"thinking","label":"Thinking","type":"boolean","required":false}
    ])).unwrap();
    if !extra {
        options.truncate(1);
    }
    harness
        .app
        .state
        .models
        .by_provider
        .entry("provider".into())
        .or_default()
        .push(kraai_runtime::Model {
            id: "reasoner".into(),
            name: "Reasoner".into(),
            max_context: None,
            options,
        });
}

fn remembered<'a>(harness: &'a TestHarness, model: &str, id: &str) -> Option<&'a ModelOptionValue> {
    harness
        .app
        .state
        .models
        .remembered_options
        .get("provider")?
        .get(model)?
        .get(id)
}

#[test]
fn arrows_change_reasoning_effort_on_the_highlighted_model_only() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    add_effort_model(&mut harness, false);
    let before = harness.app.state.models.selected_options.clone();

    harness.app.handle_command("model");
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(!rendered.contains("low  medium  high"));
    assert!(!rendered.contains("←→ effort"));
    key(&mut harness, KeyCode::Left);
    assert_eq!(harness.app.state.models.selected_options, before);

    key(&mut harness, KeyCode::Down);
    let rendered = screen(&harness.app.state, 100, 30);
    assert!(rendered.contains("Reasoner  low  medium  high"));
    assert!(rendered.contains("tab or ↑↓ select · ←→ effort · enter use · esc close"));
    assert_eq!(
        remembered(&harness, "reasoner", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("medium".into()))
    );
    key(&mut harness, KeyCode::Left);
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
    assert_eq!(harness.app.state.sheet.index, 1);
    assert_eq!(
        harness.app.state.models.selected_model_id.as_deref(),
        Some("model")
    );
    assert_eq!(harness.app.state.models.selected_options, before);

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
        harness.app.state.models.selected_model_id.as_deref(),
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
        harness.app.state.sheet.notice.as_deref(),
        Some("Reasoner has no other settings")
    );
}

#[test]
fn highlighted_model_keeps_its_other_settings_beside_the_effort_choices() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    add_effort_model(&mut harness, true);
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
    assert!(screen(&harness.app.state, 100, 30).contains("Reasoner  low, thinking"));
}

#[test]
fn a_highlighted_model_starts_on_medium_and_models_never_highlighted_stay_unset() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    add_effort_model(&mut harness, true);
    let models = harness
        .app
        .state
        .models
        .by_provider
        .entry("provider".into())
        .or_default();
    let mut fresh = models.last().cloned().expect("missing model");
    fresh.id = "fresh".into();
    fresh.name = "Fresh".into();
    models.push(fresh);
    let row = |harness: &TestHarness, name: &str| {
        screen_rows(&harness.app.state, 100, 30)
            .into_iter()
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
        harness.app.state.models.selected_model_id.as_deref(),
        Some("reasoner")
    );
    let medium = ModelOptionValues::from([(
        "reasoning_effort".into(),
        ModelOptionValue::Choice("medium".into()),
    )]);
    assert_eq!(harness.app.state.models.selected_options, medium);

    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Right);
    ctrl(&mut harness, 's');
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Right);
    key(&mut harness, KeyCode::Right);
    key(&mut harness, KeyCode::Esc);
    assert_eq!(
        harness.app.state.models.selected_options,
        ModelOptionValues::from([
            (
                "reasoning_effort".into(),
                ModelOptionValue::Choice("high".into())
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
        harness.app.state.models.selected_model_id.as_deref(),
        Some("fresh")
    );
    assert_eq!(harness.app.state.models.selected_options, medium);
    assert_eq!(
        remembered(&harness, "reasoner", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
}

#[test]
fn an_effort_that_a_toggle_switches_on_starts_in_the_middle_of_its_list() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    harness
        .app
        .state
        .models.by_provider
        .entry("provider".into())
        .or_default()
        .push(kraai_runtime::Model {
            id: "gated".into(),
            name: "Gated".into(),
            max_context: None,
            options: serde_json::from_value(serde_json::json!([
                {"id":"reasoning_enabled","label":"Thinking enabled","type":"boolean","required":true},
                {"id":"reasoning_effort","label":"Reasoning effort","type":"choice","required":true,"active_when":{"option":"reasoning_enabled","value":true},"binding":{"type":"body","path":"/effort"},"choices":[{"id":"minimal","label":"minimal"},{"id":"low","label":"low"},{"id":"high","label":"high"},{"id":"xhigh","label":"xhigh"}]}
            ])).unwrap(),
        });
    harness.app.handle_command("model");
    key(&mut harness, KeyCode::Up);
    assert!(
        harness
            .app
            .state
            .model_option_values("provider", "gated")
            .is_empty()
    );
    ctrl(&mut harness, 's');
    key(&mut harness, KeyCode::Right);
    assert_eq!(remembered(&harness, "gated", "reasoning_effort"), None);
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        remembered(&harness, "gated", "reasoning_enabled"),
        Some(&ModelOptionValue::Boolean(true))
    );
    assert_eq!(
        remembered(&harness, "gated", "reasoning_effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
}

#[test]
fn tab_and_shift_tab_walk_the_model_list_and_the_settings() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    let mut second = model_mut(&mut harness).clone();
    second.id = "second".into();
    harness
        .app
        .state
        .models
        .by_provider
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
        assert_eq!(harness.app.state.sheet.index, expected);
        assert_eq!(harness.app.state.mode, UiMode::Model);
    }
    ctrl(&mut harness, 's');
    for (code, expected) in [
        (KeyCode::Tab, 1),
        (KeyCode::BackTab, 0),
        (KeyCode::BackTab, 2),
    ] {
        key(&mut harness, code);
        assert_eq!(harness.app.state.sheet.index, expected);
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
}

#[test]
fn each_model_remembers_its_own_settings() {
    let mut harness = test_harness();
    configure_optional(&mut harness);
    let mut second = model_mut(&mut harness).clone();
    second.id = "second".into();
    second.name = "Second".into();
    harness
        .app
        .state
        .models
        .by_provider
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
        harness.app.state.models.selected_model_id.as_deref(),
        Some("model")
    );
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert_eq!(
        harness.app.state.models.selected_model_id.as_deref(),
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
        harness.app.state.models.selected_model_id.as_deref(),
        Some("model")
    );
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
}

#[test]
fn missing_options_block_submission_without_selecting_defaults() {
    let mut harness = test_harness();
    configure(&mut harness);
    harness.app.submit_message("Hello".into());
    assert!(harness.drain_requests().is_empty());
    assert!(harness.app.state.models.selected_options.is_empty());
    let error = harness.app.state.errors.last.clone().unwrap_or_default();
    assert!(error.contains("effort"));
    assert!(error.contains("thinking"));
}

#[test]
fn typed_options_validate_and_hide_inactive_budget() {
    let mut harness = test_harness();
    configure(&mut harness);
    assert!(set(&mut harness, "effort", "high").is_ok());
    assert!(set(&mut harness, "thinking", "true").is_ok());
    assert!(set(&mut harness, "budget", "101").is_err());
    assert!(set(&mut harness, "budget", "50").is_ok());
    assert_eq!(
        harness.app.state.models.selected_options.get("budget"),
        Some(&ModelOptionValue::Integer(50))
    );
    assert!(set(&mut harness, "thinking", "false").is_ok());
    assert!(
        !harness
            .app
            .state
            .models
            .selected_options
            .contains_key("budget")
    );
    assert!(set(&mut harness, "budget", "50").is_err());
    assert_eq!(active_ids(&harness), ["effort", "thinking"]);
}
