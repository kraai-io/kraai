use super::super::super::providers_sheet::{CONNECT_LABEL, ProviderAction};
use super::super::super::{AppState, ProviderAuthState, ProviderAuthStatus, ProvidersView};
use super::super::sheet::{Note, Sheet, SheetItem, SheetRow, Tone};
use super::{age, notice, plural, provider_config};

pub(super) fn providers(state: &AppState) -> Sheet {
    match state.providers_view {
        ProvidersView::List => list(state),
        ProvidersView::Connect => connect(state),
        ProvidersView::Provider => provider(state),
        ProvidersView::Config => provider_config::config(state),
    }
}

pub(super) fn hints(state: &AppState) -> String {
    match (state.providers_view, state.sheet_armed) {
        (ProvidersView::List, true) => {
            String::from("del remove this provider · any other key keeps it")
        }
        (ProvidersView::List, false) => {
            String::from("tab or ↑↓ select · enter open · del remove · ctrl+r refresh · esc close")
        }
        (ProvidersView::Connect, _) => String::from("tab or ↑↓ select · enter connect · esc back"),
        (ProvidersView::Provider, true) => {
            String::from("enter remove this provider · any other key keeps it")
        }
        (ProvidersView::Provider, false) => String::from("tab or ↑↓ select · enter run · esc back"),
        (ProvidersView::Config, _) => provider_config::hints(state),
    }
}

fn display_name<'a>(state: &'a AppState, type_id: &'a str) -> &'a str {
    state
        .provider_definition(type_id)
        .map_or(type_id, |definition| definition.display_name.as_str())
}

fn model_count(state: &AppState, provider_id: &str) -> String {
    plural(
        state
            .models_by_provider
            .get(provider_id)
            .map_or(0, Vec::len),
        "model",
    )
}

/// Where a sign-in stands, in a few words.
fn auth_words(auth: &ProviderAuthStatus) -> (String, Tone) {
    match auth.state {
        ProviderAuthState::SignedOut => (String::from("signed out"), Tone::Waiting),
        ProviderAuthState::BrowserPending => (String::from("signing in"), Tone::Waiting),
        ProviderAuthState::DeviceCodePending => (String::from("awaiting code"), Tone::Waiting),
        ProviderAuthState::Authenticated => (
            auth.plan_type
                .as_ref()
                .map_or_else(|| String::from("signed in"), |plan| format!("{plan} plan")),
            Tone::Plain,
        ),
    }
}

fn list(state: &AppState) -> Sheet {
    let providers = state.listed_providers();
    let total = providers.len() + usize::from(state.offers_connect());
    let highlighted = state.sheet_index.min(total.saturating_sub(1));
    let mut rows: Vec<_> = providers
        .iter()
        .enumerate()
        .map(|(row, (_, provider))| {
            let (tag, tone) = if state.sheet_armed && row == highlighted {
                (String::from("del again to remove"), Tone::Danger)
            } else {
                state
                    .provider_auth
                    .get(&provider.type_id)
                    .map(auth_words)
                    .unwrap_or_default()
            };
            SheetRow::Item(SheetItem {
                label: provider.id.clone(),
                detail: display_name(state, &provider.type_id).to_owned(),
                meta: model_count(state, &provider.id),
                tag,
                tone,
                ..SheetItem::default()
            })
        })
        .collect();
    if state.offers_connect() {
        rows.push(SheetRow::Item(SheetItem {
            label: String::from(CONNECT_LABEL),
            ..SheetItem::default()
        }));
    }
    Sheet {
        caption: String::from("providers"),
        count: plural(providers.len(), "provider"),
        rows,
        selected: state.sheet_index,
        empty: String::from(if state.settings_draft.is_none() {
            "Loading providers"
        } else {
            "No providers match"
        }),
        notes: notice(state),
        align_choices: true,
    }
}

fn connect(state: &AppState) -> Sheet {
    let definitions = state.listed_provider_definitions();
    let highlighted = definitions.get(state.sheet_index.min(definitions.len().saturating_sub(1)));
    let mut notes = Vec::new();
    if let Some(definition) = highlighted {
        notes.push(Note::default());
        notes.push(Note::new(&definition.description, Tone::Plain));
    }
    notes.extend(notice(state));
    Sheet {
        caption: String::from("connect"),
        count: plural(definitions.len(), "kind"),
        rows: definitions
            .iter()
            .map(|definition| {
                SheetRow::Item(SheetItem {
                    label: definition.display_name.clone(),
                    ..SheetItem::default()
                })
            })
            .collect(),
        selected: state.sheet_index,
        empty: String::from(if state.provider_definitions.is_empty() {
            "Loading the kinds of provider"
        } else {
            "No kind of provider matches"
        }),
        notes,
        align_choices: true,
    }
}

fn provider(state: &AppState) -> Sheet {
    let Some(provider) = state.open_provider() else {
        return Sheet {
            caption: String::from("provider"),
            count: String::new(),
            rows: Vec::new(),
            selected: 0,
            empty: String::from("This provider is no longer configured"),
            notes: notice(state),
            align_choices: true,
        };
    };
    let auth = state.open_provider_auth();
    let auth_state = auth.map(|auth| auth.state);
    let actions = state.listed_provider_actions();
    let highlighted = state.sheet_index.min(actions.len().saturating_sub(1));

    let mut summary = vec![display_name(state, &provider.type_id).to_owned()];
    summary.extend(auth.map(|auth| auth_words(auth).0));
    summary.push(model_count(state, &provider.id));
    if let Some(refreshed) = auth
        .and_then(|auth| auth.last_refresh.as_ref())
        .and_then(|refreshed| refreshed.parse::<u64>().ok())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        summary.push(format!(
            "refreshed {} ago",
            age(now.saturating_sub(refreshed)).trim_start()
        ));
    }

    let mut rows = vec![SheetRow::Heading(summary.join(" · "))];
    if actions.is_empty() {
        rows.push(SheetRow::Heading(String::from("No actions match")));
    }
    rows.extend(actions.iter().enumerate().map(|(row, action)| {
        let armed = state.sheet_armed && row == highlighted && *action == ProviderAction::Remove;
        SheetRow::Item(SheetItem {
            label: action.label(auth_state).to_owned(),
            tag: if armed {
                String::from("enter again to remove")
            } else {
                String::new()
            },
            tone: if *action == ProviderAction::Remove {
                Tone::Danger
            } else {
                Tone::Plain
            },
            ..SheetItem::default()
        })
    }));

    let mut notes = Vec::new();
    if let Some(auth) = auth {
        match auth.state {
            ProviderAuthState::BrowserPending => {
                notes.push(Note::new("Complete sign-in in your browser.", Tone::Plain));
                notes.extend(auth.auth_url.as_ref().map(|url| Note::field("url", url)));
            }
            ProviderAuthState::DeviceCodePending => {
                notes.push(Note::new(
                    "Enter the code on the page your browser opened.",
                    Tone::Plain,
                ));
                notes.extend(
                    auth.user_code
                        .as_ref()
                        .map(|code| Note::field("code", code)),
                );
                notes.extend(
                    auth.verification_url
                        .as_ref()
                        .map(|url| Note::field("url", url)),
                );
            }
            ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => {}
        }
        notes.extend(
            auth.error
                .as_ref()
                .map(|error| Note::new(error, Tone::Danger)),
        );
    }
    notes.extend(notice(state));
    Sheet {
        caption: provider.id.clone(),
        count: String::new(),
        rows,
        selected: state.sheet_index,
        empty: String::new(),
        notes,
        align_choices: true,
    }
}
