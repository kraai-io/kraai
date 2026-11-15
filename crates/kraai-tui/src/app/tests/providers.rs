use kraai_runtime::{
    FieldDefinition, FieldValueEntry, FieldValueKind, FieldViolation, Model, ModelSettings,
    ProviderDefinition, ProviderSettings, RuntimeError, RuntimeErrorKind, SettingsDocument,
    SettingsValue,
};

use super::*;
use crate::app::auth::{ProviderAuthState, ProviderAuthStatus};
use crate::app::{KeyCode, ProvidersView, UiMode};

fn field(key: &str, label: &str, value_kind: FieldValueKind, required: bool) -> FieldDefinition {
    FieldDefinition {
        key: key.into(),
        label: label.into(),
        value_kind,
        required,
        secret: false,
        help_text: (key == "url").then(|| String::from("Where requests go")),
        default_value: None,
    }
}

fn definition(type_id: &str, name: &str, description: &str) -> ProviderDefinition {
    ProviderDefinition {
        type_id: type_id.into(),
        display_name: name.into(),
        protocol_family: String::from("openai"),
        description: description.into(),
        provider_fields: vec![
            field("url", "Base URL", FieldValueKind::Url, true),
            field("limit", "Limit", FieldValueKind::Integer, false),
            field("stream", "Stream", FieldValueKind::Boolean, false),
        ],
        model_fields: vec![field(
            "context",
            "Context window",
            FieldValueKind::Integer,
            false,
        )],
        supports_model_discovery: true,
        default_provider_id_prefix: type_id.into(),
    }
}

fn model(id: &str) -> Model {
    Model {
        id: id.into(),
        name: id.into(),
        max_context: None,
        options: Vec::new(),
    }
}

fn settings() -> SettingsDocument {
    SettingsDocument {
        providers: vec![
            ProviderSettings {
                id: String::from("codex"),
                type_id: String::from("openai-codex"),
                values: Vec::new(),
            },
            ProviderSettings {
                id: String::from("local"),
                type_id: String::from("chat"),
                values: vec![FieldValueEntry {
                    key: String::from("url"),
                    value: SettingsValue::String(String::from("http://localhost")),
                }],
            },
        ],
        models: vec![ModelSettings {
            id: String::from("small"),
            provider_id: String::from("local"),
            values: Vec::new(),
            options: Vec::new(),
            remove_options: Vec::new(),
        }],
    }
}

fn auth(state: ProviderAuthState) -> ProviderAuthStatus {
    ProviderAuthStatus {
        provider_type: String::from("openai-codex"),
        sequence: 1,
        state,
        plan_type: Some(String::from("plus")),
        ..Default::default()
    }
}

fn open_providers(harness: &mut TestHarness) {
    harness.app.state.provider_definitions = vec![
        definition(
            "openai-codex",
            "OpenAI Codex",
            "Signs in with a ChatGPT account",
        ),
        definition("chat", "Chat Completions", "Any OpenAI-compatible server"),
    ];
    harness
        .app
        .state
        .models_by_provider
        .insert(String::from("local"), vec![model("a"), model("b")]);
    harness.app.state.provider_auth.insert(
        String::from("openai-codex"),
        auth(ProviderAuthState::Authenticated),
    );
    harness.app.handle_command("providers");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Settings(Ok(settings())));
    harness.drain_requests();
}

fn saved(harness: &TestHarness) -> Vec<SettingsDocument> {
    harness
        .drain_requests()
        .into_iter()
        .filter_map(|request| match request {
            RuntimeRequest::SaveSettings { settings } => Some(settings),
            _ => None,
        })
        .collect()
}

fn open_config(harness: &mut TestHarness) {
    open_providers(harness);
    key(harness, KeyCode::Down);
    key(harness, KeyCode::Enter);
    type_text(harness, "edit");
    key(harness, KeyCode::Enter);
    harness.drain_requests();
    assert_eq!(harness.app.state.providers_view, ProvidersView::Config);
}

#[test]
fn provider_list_waits_for_settings_then_lists_providers_and_a_connect_row() {
    let mut harness = test_harness();
    harness.app.handle_command("providers");
    assert_eq!(harness.app.state.mode, UiMode::Providers);
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [
            RuntimeRequest::ListProviderDefinitions,
            RuntimeRequest::GetSettings,
            RuntimeRequest::GetProviderAuthStatuses,
        ]
    ));
    assert_eq!(
        sheet(&harness, 70).get(1).map(String::as_str),
        Some("  Loading providers")
    );
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);

    let mut harness = test_harness();
    open_providers(&mut harness);
    assert_eq!(
        sheet(&harness, 70),
        [
            format!("─ providers {} 2 providers ─", "─".repeat(44)),
            spaced("▌ codex  OpenAI Codex", "plus plan  0 models", 70),
            spaced("  local  Chat Completions", "2 models", 70),
            String::from("  Connect a provider"),
        ]
    );
    assert_eq!(
        hints(&harness),
        "tab or ↑↓ select · enter open · del remove · ctrl+r refresh · esc close"
    );
    assert_eq!(composer(&harness), "/providers");
    let [_, status, _] = screen_parts(&harness, 80);
    assert_eq!(
        status,
        [
            " tab or ↑↓ select · enter open · del remove · ctrl+r refresh · esc close",
            " No model selected  0 tokens  $0.0000",
        ]
    );

    ctrl(&mut harness, 'r');
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::ListModels]
    ));
    assert_eq!(
        sheet(&harness, 70).last().map(String::as_str),
        Some("  Refreshing models")
    );
    type_text(&mut harness, "chat");
    assert_eq!(
        sheet(&harness, 70).get(1..).unwrap_or_default(),
        [spaced("▌ local  Chat Completions", "2 models", 70)]
    );
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.sheet_query.is_empty());
}

#[test]
fn a_provider_offers_the_actions_its_sign_in_allows_and_typing_narrows_them() {
    let mut harness = test_harness();
    open_providers(&mut harness);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.providers_view, ProvidersView::Provider);
    assert_eq!(
        sheet(&harness, 70),
        [
            format!("─ codex {}", "─".repeat(62)),
            String::from("  OpenAI Codex · plus plan · 0 models"),
            String::from("▌ Refresh models"),
            String::from("  Edit configuration"),
            String::from("  Sign in again with the browser"),
            String::from("  Sign in again with a device code"),
            String::from("  Log out"),
            String::from("  Remove provider"),
        ]
    );
    assert_eq!(hints(&harness), "tab or ↑↓ select · enter run · esc back");
    assert_eq!(composer(&harness), "/providers › codex ›");
    harness.drain_requests();

    type_text(&mut harness, "log");
    key(&mut harness, KeyCode::Enter);
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::LogoutProvider { provider_type }] if provider_type == "openai-codex"
    ));
    assert_eq!(
        sheet(&harness, 70).get(2..).unwrap_or_default(),
        [String::from("▌ Log out"), String::from("  Logging out")]
    );

    let mut pending = auth(ProviderAuthState::DeviceCodePending);
    pending.sequence = 2;
    pending.user_code = Some(String::from("ABCD-1234"));
    harness
        .app
        .state
        .provider_auth
        .insert(pending.provider_type.clone(), pending);
    ctrl(&mut harness, 'u');
    assert_eq!(
        sheet(&harness, 70).get(1..).unwrap_or_default(),
        [
            String::from("  OpenAI Codex · awaiting code · 0 models"),
            String::from("▌ Open the browser again"),
            String::from("  Copy the device code"),
            String::from("  Cancel sign-in"),
            String::from("  Refresh models"),
            String::from("  Edit configuration"),
            String::from("  Remove provider"),
            String::from("  Enter the code on the page your browser opened."),
            String::from("  code  ABCD-1234"),
        ]
    );

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert!(matches!(
        harness.drain_requests().as_slice(),
        [RuntimeRequest::CancelProviderLogin { .. }]
    ));
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);
    assert_eq!(harness.app.state.sheet_index, 0);

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        sheet(&harness, 70).get(..5).unwrap_or_default(),
        [
            format!("─ local {}", "─".repeat(62)),
            String::from("  Chat Completions · 2 models"),
            String::from("▌ Refresh models"),
            String::from("  Edit configuration"),
            String::from("  Remove provider"),
        ]
    );
    key(&mut harness, KeyCode::Backspace);
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);
    assert_eq!(harness.app.state.sheet_index, 1);
}

#[test]
fn removing_a_provider_asks_twice_from_the_list_and_from_its_own_sheet() {
    let mut harness = test_harness();
    open_providers(&mut harness);
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Delete);
    assert!(saved(&harness).is_empty());
    assert_eq!(
        sheet(&harness, 70).get(2),
        Some(&spaced(
            "▌ local  Chat Completions",
            "del again to remove  2 models",
            70
        ))
    );
    assert_eq!(
        hints(&harness),
        "del remove this provider · any other key keeps it"
    );
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Down);
    assert!(!harness.app.state.sheet_armed);
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Delete);
    assert!(!harness.app.state.sheet_armed);
    key(&mut harness, KeyCode::Up);

    key(&mut harness, KeyCode::Delete);
    key(&mut harness, KeyCode::Delete);
    let documents = saved(&harness);
    assert_eq!(documents.len(), 1);
    let document = documents.first().cloned().unwrap_or_default();
    assert_eq!(document.providers.len(), 1);
    assert!(document.models.is_empty());
    assert_eq!(
        sheet(&harness, 70).last().map(String::as_str),
        Some("  Removed local")
    );

    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Enter);
    type_text(&mut harness, "remove");
    key(&mut harness, KeyCode::Enter);
    assert!(saved(&harness).is_empty());
    assert_eq!(
        sheet(&harness, 70).get(2),
        Some(&spaced("▌ Remove provider", "enter again to remove", 70))
    );
    assert_eq!(
        hints(&harness),
        "enter remove this provider · any other key keeps it"
    );
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    assert!(saved(&harness).is_empty());
    key(&mut harness, KeyCode::Enter);
    assert!(
        saved(&harness)
            .first()
            .is_some_and(|document| document.providers.is_empty())
    );
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);
    assert_eq!(
        sheet(&harness, 70).get(1..).unwrap_or_default(),
        [
            String::from("▌ Connect a provider"),
            String::from("  Removed codex")
        ]
    );
}

#[test]
fn connecting_lists_the_kinds_of_provider_and_opens_the_new_one() {
    let mut harness = test_harness();
    open_providers(&mut harness);
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.providers_view, ProvidersView::Connect);
    assert_eq!(
        sheet(&harness, 70),
        [
            format!("─ connect {} 2 kinds ─", "─".repeat(50)),
            String::from("▌ OpenAI Codex"),
            String::from("  Chat Completions"),
            String::new(),
            String::from("  Signs in with a ChatGPT account"),
        ]
    );
    assert_eq!(
        hints(&harness),
        "tab or ↑↓ select · enter connect · esc back"
    );
    assert_eq!(composer(&harness), "/providers › connect ›");
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.providers_view, ProvidersView::List);
    assert_eq!(harness.app.state.sheet_index, 2);

    key(&mut harness, KeyCode::Enter);
    type_text(&mut harness, "chat");
    assert_eq!(
        sheet(&harness, 70).get(1..).unwrap_or_default(),
        [
            String::from("▌ Chat Completions"),
            String::new(),
            String::from("  Any OpenAI-compatible server"),
        ]
    );
    harness.drain_requests();
    key(&mut harness, KeyCode::Enter);
    let documents = saved(&harness);
    let added = documents
        .first()
        .and_then(|document| document.providers.last())
        .cloned();
    assert_eq!(
        added.map(|provider| (provider.id, provider.type_id)),
        Some((String::from("chat"), String::from("chat")))
    );
    assert_eq!(harness.app.state.providers_view, ProvidersView::Provider);
    assert_eq!(composer(&harness), "/providers › chat ›");
    assert_eq!(
        sheet(&harness, 70).last().map(String::as_str),
        Some("  Connected chat")
    );
}

#[test]
fn configuration_is_one_form_edited_in_the_composer() {
    let mut harness = test_harness();
    open_config(&mut harness);
    assert_eq!(
        sheet(&harness, 70),
        [
            format!("─ local {} configuration ─", "─".repeat(46)),
            String::from("  Provider"),
            String::from("▌ ID              local"),
            String::from("  Type            OpenAI Codex  Chat Completions"),
            String::from("  Base URL        http://localhost"),
            String::from("  Limit           unset"),
            String::from("  Stream          no  yes"),
            String::from("  Models"),
            String::from("  Add a model"),
            String::from("  small"),
            String::from("  ID              small"),
            String::from("  Context window  unset"),
        ]
    );
    assert_eq!(hints(&harness), "tab or ↑↓ select · enter edit · esc back");
    assert_eq!(composer(&harness), "/providers › local › config ›");

    key(&mut harness, KeyCode::Enter);
    assert_eq!(hints(&harness), "enter save · esc cancel");
    assert_eq!(
        composer(&harness),
        "/providers › local › config › ID = local"
    );
    type_text(&mut harness, "2");
    assert_eq!(
        sheet(&harness, 70).get(2).map(String::as_str),
        Some("▌ ID              local2")
    );
    key(&mut harness, KeyCode::Down);
    assert_eq!(harness.app.state.sheet_index, 0);
    key(&mut harness, KeyCode::Enter);
    let document = saved(&harness).pop().unwrap_or_default();
    assert_eq!(
        document
            .providers
            .get(1)
            .map(|provider| provider.id.as_str()),
        Some("local2")
    );
    assert_eq!(
        document
            .models
            .first()
            .map(|model| model.provider_id.as_str()),
        Some("local2")
    );
    assert_eq!(composer(&harness), "/providers › local2 › config ›");

    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Down);
    assert_eq!(
        sheet(&harness, 70).last().map(String::as_str),
        Some("  Where requests go")
    );
    key(&mut harness, KeyCode::Down);
    type_text(&mut harness, "x1");
    assert_eq!(
        composer(&harness),
        "/providers › local2 › config › Limit = x1"
    );
    key(&mut harness, KeyCode::Enter);
    assert!(saved(&harness).is_empty());
    assert_eq!(
        sheet(&harness, 70).last().map(String::as_str),
        Some("  Limit takes a whole number")
    );
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.providers_view, ProvidersView::Config);
    assert!(harness.app.state.config_editing.is_none());
    assert!(harness.app.state.sheet_query.is_empty());
    harness.app.handle_paste(String::from("25"));
    key(&mut harness, KeyCode::Enter);
    let limit = |document: &SettingsDocument| {
        document.providers.get(1).and_then(|provider| {
            provider
                .values
                .iter()
                .find(|entry| entry.key == "limit")
                .map(|entry| entry.value.clone())
        })
    };
    assert_eq!(
        saved(&harness).last().and_then(limit),
        Some(SettingsValue::Integer(25))
    );
    key(&mut harness, KeyCode::Enter);
    ctrl(&mut harness, 'u');
    key(&mut harness, KeyCode::Enter);
    assert_eq!(saved(&harness).last().and_then(limit), None);
}

#[test]
fn a_secret_is_pasted_and_saved_without_ever_reaching_the_screen() {
    let mut harness = test_harness();
    open_providers(&mut harness);
    for definition in &mut harness.app.state.provider_definitions {
        definition.provider_fields.push(FieldDefinition {
            secret: true,
            ..field("key", "API key", FieldValueKind::SecretString, false)
        });
    }
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Enter);
    type_text(&mut harness, "edit");
    key(&mut harness, KeyCode::Enter);
    harness.drain_requests();
    for _ in 0..5 {
        key(&mut harness, KeyCode::Down);
    }
    let row = |harness: &TestHarness| sheet(harness, 70).get(7).cloned().unwrap_or_default();
    let shown = |harness: &TestHarness| screen_rows(&harness.app.state, 120, 40).join("\n");
    let stored = |document: &SettingsDocument| {
        document.providers.get(1).and_then(|provider| {
            provider
                .values
                .iter()
                .find(|entry| entry.key == "key")
                .map(|entry| entry.value.clone())
        })
    };
    assert_eq!(row(&harness), "▌ API key         unset");

    harness
        .app
        .handle_paste(String::from("sk-test-1234567890abcd\r\n"));
    assert_eq!(row(&harness), "▌ API key         22 characters");
    assert_eq!(
        composer(&harness),
        format!("/providers › local › config › API key = {}", "•".repeat(22))
    );
    assert!(!shown(&harness).contains("sk-test"));
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        saved(&harness).last().and_then(stored),
        Some(SettingsValue::String(String::from(
            "sk-test-1234567890abcd"
        )))
    );
    assert_eq!(row(&harness), "▌ API key         ••••••••abcd");
    assert!(!shown(&harness).contains("sk-test"));

    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        composer(&harness),
        "/providers › local › config › API key ="
    );
    assert_eq!(row(&harness), "▌ API key         ••••••••abcd");
    key(&mut harness, KeyCode::Enter);
    assert!(saved(&harness).is_empty());
    assert_eq!(
        row(&harness),
        spaced("▌ API key         ••••••••abcd", "enter again to clear", 70)
    );
    assert_eq!(
        hints(&harness),
        "enter clear the saved value · any other key keeps it"
    );
    type_text(&mut harness, "short");
    assert_eq!(row(&harness), "▌ API key         5 characters");
    assert_eq!(hints(&harness), "enter save · esc cancel");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        saved(&harness).last().and_then(stored),
        Some(SettingsValue::String(String::from("short")))
    );
    assert_eq!(row(&harness), "▌ API key         ••••••••");

    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Esc);
    assert!(!harness.app.state.sheet_armed);
    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Enter);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(saved(&harness).last().map(stored), Some(None));
    assert_eq!(row(&harness), "▌ API key         unset");
}

#[test]
fn toggles_and_the_type_step_in_place_and_models_are_added_and_removed() {
    let mut harness = test_harness();
    open_config(&mut harness);
    for _ in 0..4 {
        key(&mut harness, KeyCode::Down);
    }
    assert_eq!(hints(&harness), "tab or ↑↓ select · ←→ change · esc back");
    key(&mut harness, KeyCode::Right);
    let stream = |document: &SettingsDocument| {
        document.providers.get(1).and_then(|provider| {
            provider
                .values
                .iter()
                .find(|entry| entry.key == "stream")
                .map(|entry| entry.value.clone())
        })
    };
    assert_eq!(
        saved(&harness).last().and_then(stream),
        Some(SettingsValue::Bool(true))
    );
    type_text(&mut harness, "x");
    assert!(harness.app.state.config_editing.is_none());
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        saved(&harness).last().and_then(stream),
        Some(SettingsValue::Bool(false))
    );

    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Up);
    key(&mut harness, KeyCode::Left);
    assert_eq!(
        saved(&harness)
            .last()
            .and_then(|document| document.providers.get(1))
            .map(|provider| provider.type_id.as_str()),
        Some("openai-codex")
    );
    key(&mut harness, KeyCode::Right);
    saved(&harness);

    for _ in 0..4 {
        key(&mut harness, KeyCode::Down);
    }
    assert_eq!(hints(&harness), "tab or ↑↓ select · enter add · esc back");
    key(&mut harness, KeyCode::Delete);
    assert!(!harness.app.state.sheet_armed);
    key(&mut harness, KeyCode::Enter);
    let document = saved(&harness).pop().unwrap_or_default();
    assert_eq!(
        document
            .models
            .iter()
            .map(|model| (model.id.as_str(), model.provider_id.as_str()))
            .collect::<Vec<_>>(),
        [("small", "local"), ("model-1", "local")]
    );
    assert_eq!(
        sheet(&harness, 70).get(12..).unwrap_or_default(),
        [
            String::from("  model-1"),
            String::from("▌ ID              model-1"),
            String::from("  Context window  unset"),
        ]
    );
    assert_eq!(
        hints(&harness),
        "tab or ↑↓ select · enter edit · del remove model · esc back"
    );

    key(&mut harness, KeyCode::Delete);
    assert_eq!(
        sheet(&harness, 70).get(13),
        Some(&spaced(
            "▌ ID              model-1",
            "del again to remove",
            70
        ))
    );
    assert_eq!(
        hints(&harness),
        "del remove this model · any other key keeps it"
    );
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Delete);
    assert!(saved(&harness).is_empty());
    key(&mut harness, KeyCode::Delete);
    let document = saved(&harness).pop().unwrap_or_default();
    assert_eq!(document.models.len(), 1);
    assert_eq!(
        sheet(&harness, 70).last().map(String::as_str),
        Some("  Removed model model-1")
    );
}

#[test]
fn a_refused_save_marks_the_row_and_says_why_when_it_is_highlighted() {
    let mut harness = test_harness();
    open_config(&mut harness);
    let mut error = RuntimeError::new(RuntimeErrorKind::Validation, "invalid settings");
    error.violations = vec![FieldViolation {
        field: String::from("providers[1].url"),
        message: String::from("Base URL must start with http"),
    }];
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SaveSettings(Err(error)));
    assert_eq!(
        sheet(&harness, 70).get(4),
        Some(&spaced("  Base URL        http://localhost", "invalid", 70))
    );
    key(&mut harness, KeyCode::Down);
    key(&mut harness, KeyCode::Down);
    assert_eq!(
        sheet(&harness, 70).get(12..).unwrap_or_default(),
        [
            String::from("  Where requests go"),
            String::from("  Base URL must start with http"),
        ]
    );
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SaveSettings(Ok(())));
    assert!(harness.app.state.settings_errors.is_empty());
    key(&mut harness, KeyCode::Esc);
    assert_eq!(harness.app.state.providers_view, ProvidersView::Provider);
    assert_eq!(
        sheet(&harness, 70).get(3).map(String::as_str),
        Some("▌ Edit configuration")
    );
}

fn highlight(harness: &mut TestHarness, row: &crate::app::provider_config::ConfigRow) {
    if let Some(index) = harness
        .app
        .state
        .config_rows()
        .iter()
        .position(|candidate| candidate == row)
    {
        harness.app.state.sheet_index = index;
    }
}

fn draft_ids(harness: &TestHarness) -> (Vec<String>, Vec<(String, String)>) {
    let draft = harness.app.state.settings_draft.clone().unwrap_or_default();
    (
        draft
            .providers
            .into_iter()
            .map(|provider| provider.id)
            .collect(),
        draft
            .models
            .into_iter()
            .map(|model| (model.id, model.provider_id))
            .collect(),
    )
}

#[test]
fn a_provider_cannot_take_the_id_of_another_provider() {
    use crate::app::provider_config::ConfigRow;
    let mut harness = test_harness();
    open_config(&mut harness);
    highlight(&mut harness, &ConfigRow::ProviderId);
    key(&mut harness, KeyCode::Enter);
    harness.app.state.sheet_query = String::from("codex");
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        harness.app.state.sheet_notice.as_deref(),
        Some("A provider is already named codex")
    );
    assert!(saved(&harness).is_empty());
    assert_eq!(
        draft_ids(&harness),
        (
            vec![String::from("codex"), String::from("local")],
            vec![(String::from("small"), String::from("local"))]
        )
    );
}

#[test]
fn a_new_model_takes_the_first_id_its_provider_has_free() {
    use crate::app::provider_config::ConfigRow;
    let mut harness = test_harness();
    open_config(&mut harness);
    let model_ids = |harness: &TestHarness| {
        draft_ids(harness)
            .1
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>()
    };
    for _ in 0..2 {
        highlight(&mut harness, &ConfigRow::AddModel);
        key(&mut harness, KeyCode::Enter);
    }
    assert_eq!(model_ids(&harness), ["small", "model-1", "model-2"]);
    highlight(&mut harness, &ConfigRow::ModelId(1));
    key(&mut harness, KeyCode::Delete);
    key(&mut harness, KeyCode::Delete);
    assert_eq!(model_ids(&harness), ["small", "model-2"]);
    highlight(&mut harness, &ConfigRow::AddModel);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(model_ids(&harness), ["small", "model-2", "model-1"]);
}
