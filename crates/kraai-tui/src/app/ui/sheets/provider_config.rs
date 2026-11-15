use super::super::super::AppState;
use super::super::super::provider_config::{ConfigKind, ConfigRow, masked};
use super::super::sheet::{Note, Sheet, SheetItem, SheetRow, Tone};
use super::notice;

pub(super) fn hints(state: &AppState) -> String {
    if state.config_editing.is_some() {
        return String::from(if state.sheet_armed {
            "enter clear the saved value · any other key keeps it"
        } else {
            "enter save · esc cancel"
        });
    }
    if state.sheet_armed {
        return String::from("del remove this model · any other key keeps it");
    }
    let Some(row) = state.highlighted_config_row() else {
        return String::from("esc back");
    };
    let change = match state.config_kind(&row) {
        ConfigKind::Text => "enter edit",
        ConfigKind::Toggle | ConfigKind::Type => "←→ change",
        ConfigKind::AddModel => "enter add",
    };
    let remove = if row.model().is_some() {
        " · del remove model"
    } else {
        ""
    };
    format!("tab or ↑↓ select · {change}{remove} · esc back")
}

pub(super) fn config(state: &AppState) -> Sheet {
    let rows = state.config_rows();
    let highlighted = state.sheet_index.min(rows.len().saturating_sub(1));
    let mut sheet_rows = Vec::with_capacity(rows.len() + 4);
    for (index, row) in rows.iter().enumerate() {
        match row {
            ConfigRow::ProviderId => sheet_rows.push(SheetRow::Heading(String::from("Provider"))),
            ConfigRow::AddModel => sheet_rows.push(SheetRow::Heading(String::from("Models"))),
            ConfigRow::ModelId(_) => {
                let id = state.config_value(row);
                sheet_rows.push(SheetRow::Heading(if id.is_empty() {
                    String::from("Unnamed model")
                } else {
                    id
                }));
            }
            ConfigRow::ProviderType | ConfigRow::ProviderValue(_) | ConfigRow::ModelValue(..) => {}
        }
        sheet_rows.push(SheetRow::Item(item(state, row, index == highlighted)));
    }
    let mut notes = Vec::new();
    if let Some(row) = rows.get(highlighted) {
        notes.extend(
            state
                .config_field(row)
                .and_then(|field| field.help_text.as_ref())
                .map(|help| Note::new(help, Tone::Plain)),
        );
        notes.extend(
            state
                .config_error(row)
                .map(|error| Note::new(error, Tone::Danger)),
        );
    }
    notes.extend(notice(state));
    Sheet {
        caption: state
            .open_provider()
            .map(|provider| provider.id.clone())
            .unwrap_or_default(),
        count: String::from("configuration"),
        rows: sheet_rows,
        selected: state.sheet_index,
        empty: String::from("This provider is no longer configured"),
        notes,
        align_choices: true,
    }
}

fn item(state: &AppState, row: &ConfigRow, highlighted: bool) -> SheetItem {
    let kind = state.config_kind(row);
    let stored = state.config_value(row);
    let editing = state.config_editing.as_ref() == Some(row);
    let required = matches!(row, ConfigRow::ProviderId | ConfigRow::ModelId(_))
        || state.config_field(row).is_some_and(|field| field.required);
    let missing = kind == ConfigKind::Text && !editing && stored.is_empty();
    let (choices, chosen) = match kind {
        ConfigKind::Toggle => (
            vec![String::from("no"), String::from("yes")],
            Some(usize::from(stored == "yes")),
        ),
        ConfigKind::Type => (
            state
                .provider_definitions
                .iter()
                .map(|definition| definition.display_name.clone())
                .collect(),
            state
                .provider_definitions
                .iter()
                .position(|definition| definition.type_id == stored),
        ),
        ConfigKind::Text | ConfigKind::AddModel => (Vec::new(), None),
    };
    let secret = state.config_secret(row);
    let (tag, tone) = if editing && state.sheet_armed {
        ("enter again to clear", Tone::Danger)
    } else if highlighted && state.sheet_armed && row.model().is_some() {
        ("del again to remove", Tone::Danger)
    } else if state.config_error(row).is_some() {
        ("invalid", Tone::Danger)
    } else {
        ("", Tone::Plain)
    };
    SheetItem {
        label: state.config_label(row),
        value: match kind {
            ConfigKind::Text if editing && secret => typed_secret(&state.sheet_query, &stored),
            ConfigKind::Text if editing => state.sheet_query.clone(),
            ConfigKind::Text if missing && required => String::from("required"),
            ConfigKind::Text if missing => String::from("unset"),
            ConfigKind::Text if secret => masked(&stored),
            ConfigKind::Text => stored,
            ConfigKind::Toggle | ConfigKind::Type | ConfigKind::AddModel => String::new(),
        },
        choices,
        chosen,
        waiting: missing && required,
        tag: tag.to_owned(),
        tone,
        ..SheetItem::default()
    }
}

/// What a secret's row shows while its replacement is typed: how much is typed so far, or what
/// the row holds while nothing is.
fn typed_secret(typed: &str, stored: &str) -> String {
    match typed.chars().count() {
        0 if stored.is_empty() => String::from("unset"),
        0 => masked(stored),
        1 => String::from("1 character"),
        count => format!("{count} characters"),
    }
}
