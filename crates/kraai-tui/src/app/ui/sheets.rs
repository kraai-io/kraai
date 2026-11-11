use kraai_types::ModelOptionKind;

use super::super::model_options::{
    active_options, chosen_choice, inline_option, model_option_choices, option_summary,
    option_value_label, summarize_options,
};
use super::super::{AppState, UiMode};
use super::sheet::{Note, Sheet, SheetItem, SheetRow, Tone};

mod agent;
mod sessions;

/// The sheet the current mode shows above the status line, if any.
pub(super) fn active(state: &AppState) -> Option<Sheet> {
    match state.mode {
        UiMode::Chat => commands(state),
        UiMode::Model => Some(models(state)),
        UiMode::ModelSettings => Some(model_settings(state)),
        UiMode::Agent => Some(agent::agents(state)),
        UiMode::Sessions => Some(sessions::sessions(state)),
        _ => None,
    }
}

/// Key hints for the open sheet, shown in the status line.
pub(super) fn hints(state: &AppState) -> Option<String> {
    match state.mode {
        UiMode::Chat if !state.command_matches().is_empty() => {
            Some(String::from("tab or ↑↓ select · enter run · esc dismiss"))
        }
        UiMode::Model => {
            let mut hints = vec!["tab or ↑↓ select"];
            if let Some((provider, model)) = state.highlighted_model() {
                let values = state.model_option_values(provider, &model.id);
                if inline_option(model, &values).is_some() {
                    hints.push("←→ effort");
                }
                if state.model_has_more_settings(provider, &model.id) {
                    hints.push("ctrl+s settings");
                }
            }
            hints.extend(["enter use", "esc close"]);
            Some(hints.join(" · "))
        }
        UiMode::ModelSettings => {
            let target = state.model_settings.as_ref()?;
            let types_a_number = state.model_settings_view().is_some_and(|(model, values)| {
                active_options(model, &values)
                    .nth(state.sheet_index)
                    .is_some_and(|option| matches!(option.kind, ModelOptionKind::Integer { .. }))
            });
            let enter = if !state.sheet_query.trim().is_empty() {
                "set"
            } else if state.is_current_model(&target.provider_id, &target.model_id) {
                "done"
            } else {
                "use"
            };
            let change = if types_a_number {
                "type a number"
            } else {
                "←→ change"
            };
            Some(format!(
                "tab or ↑↓ select · {change} · enter {enter} · esc back"
            ))
        }
        UiMode::Agent => Some(String::from("tab or ↑↓ select · enter use · esc close")),
        UiMode::Sessions if state.sheet_armed => Some(String::from(
            "del delete this session · any other key keeps it",
        )),
        UiMode::Sessions => Some(String::from(
            "tab or ↑↓ select · enter open · del delete · esc close",
        )),
        _ => None,
    }
}

fn commands(state: &AppState) -> Option<Sheet> {
    let matches = state.command_matches();
    if matches.is_empty() {
        return None;
    }
    Some(Sheet {
        caption: String::from("commands"),
        count: String::new(),
        rows: matches
            .into_iter()
            .map(|command| {
                SheetRow::Item(SheetItem {
                    label: format!("/{}", command.name),
                    detail: command.description.to_owned(),
                    ..SheetItem::default()
                })
            })
            .collect(),
        selected: state.sheet_index,
        empty: String::new(),
        notes: Vec::new(),
        align_choices: true,
    })
}

fn models(state: &AppState) -> Sheet {
    let models = state.filtered_models();
    let mut rows = Vec::with_capacity(models.len());
    let mut heading = None;
    let highlighted = state.sheet_index.min(models.len().saturating_sub(1));
    for (index, (provider, model)) in models.iter().enumerate() {
        if heading != Some(*provider) {
            heading = Some(*provider);
            rows.push(SheetRow::Heading((*provider).to_owned()));
        }
        let current = state.is_current_model(provider, &model.id);
        let values = state.model_option_values(provider, &model.id);
        let effort = inline_option(model, &values).filter(|_| index == highlighted);
        let choices = effort.map(model_option_choices).unwrap_or_default();
        rows.push(SheetRow::Item(SheetItem {
            label: model.name.clone(),
            detail: match effort {
                Some(effort) => summarize_options(
                    active_options(model, &values).filter(|option| option.id != effort.id),
                    &values,
                ),
                None => option_summary(model, &values),
            },
            chosen: effort.and_then(|effort| chosen_choice(&choices, values.get(&effort.id))),
            waiting: effort
                .is_some_and(|effort| effort.required && !values.contains_key(&effort.id)),
            choices: choices.into_iter().map(|(_, label)| label).collect(),
            meta: model.max_context.map(compact_tokens).unwrap_or_default(),
            tag: if current {
                String::from("current")
            } else {
                String::new()
            },
            ..SheetItem::default()
        }));
    }
    Sheet {
        caption: String::from("model"),
        count: plural(models.len(), "model"),
        rows,
        align_choices: false,
        selected: state.sheet_index,
        empty: if state.models_by_provider.values().all(Vec::is_empty) {
            String::from("No models yet. Connect a provider with /providers")
        } else {
            String::from("No models match")
        },
        notes: notice(state),
    }
}

fn model_settings(state: &AppState) -> Sheet {
    let Some((model, values)) = state.model_settings_view() else {
        return Sheet {
            caption: state
                .model_settings
                .as_ref()
                .map(|target| target.model_id.clone())
                .unwrap_or_default(),
            count: String::new(),
            rows: Vec::new(),
            selected: 0,
            empty: String::from("This model is no longer available"),
            notes: notice(state),
            align_choices: true,
        };
    };
    let options: Vec<_> = active_options(model, &values).collect();
    let rows = options
        .iter()
        .map(|option| {
            let value = values.get(&option.id);
            let choices = model_option_choices(option);
            SheetRow::Item(SheetItem {
                label: option.label.clone(),
                value: if choices.is_empty() {
                    option_value_label(option, value)
                } else {
                    String::new()
                },
                chosen: chosen_choice(&choices, value),
                choices: choices.into_iter().map(|(_, label)| label).collect(),
                detail: option.description.clone().unwrap_or_default(),
                meta: match option.kind {
                    ModelOptionKind::Integer {
                        min: Some(min),
                        max: Some(max),
                    } => format!("{min} to {max}"),
                    ModelOptionKind::Integer {
                        min: Some(min),
                        max: None,
                    } => format!("{min} or more"),
                    ModelOptionKind::Integer {
                        min: None,
                        max: Some(max),
                    } => format!("up to {max}"),
                    _ => String::new(),
                },
                waiting: value.is_none() && option.required,
                ..SheetItem::default()
            })
        })
        .collect();
    Sheet {
        caption: model.name.clone(),
        count: plural(options.len(), "setting"),
        rows,
        selected: state.sheet_index,
        empty: format!("{} has no settings", model.name),
        notes: notice(state),
        align_choices: true,
    }
}

/// What the last key press could not do, as the sheet's closing note.
fn notice(state: &AppState) -> Vec<Note> {
    state
        .sheet_notice
        .iter()
        .map(|notice| Note::new(notice, Tone::Danger))
        .collect()
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

fn compact_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        let tenths = tokens / 100_000;
        if tenths.is_multiple_of(10) {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else if tokens >= 1000 {
        format!("{}k", tokens / 1000)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::compact_tokens;

    #[test]
    fn context_windows_shorten_to_thousands_and_millions() {
        assert_eq!(compact_tokens(900), "900");
        assert_eq!(compact_tokens(200_000), "200k");
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_tokens(1_050_000), "1M");
        assert_eq!(compact_tokens(2_500_000), "2.5M");
    }
}
