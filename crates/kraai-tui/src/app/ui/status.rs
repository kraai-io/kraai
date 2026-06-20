use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::super::AppState;
use super::STATUSLINE_STREAMING_FRAMES;

pub(super) fn statusline_line(state: &AppState) -> Line<'static> {
    let separator = Span::styled(" · ", Style::default().fg(Color::DarkGray));
    let mut spans = vec![
        Span::styled(
            statusline_activity_label(state),
            Style::default().fg(statusline_activity_color(state)),
        ),
        separator.clone(),
        Span::raw(statusline_model_label(state)),
        separator.clone(),
        Span::raw(statusline_agent_label(state)),
    ];

    spans.push(separator.clone());
    spans.push(Span::raw(statusline_context_label(state)));
    spans.push(separator);
    spans.push(Span::raw(state.status.clone()));
    Line::from(spans)
}

fn statusline_activity_label(state: &AppState) -> String {
    if state.runtime_is_active() {
        return STATUSLINE_STREAMING_FRAMES
            [state.statusline_animation_frame % STATUSLINE_STREAMING_FRAMES.len()]
        .to_string();
    }
    if state.status == "Stream cancelled" {
        return String::from("cancelled");
    }
    String::from("idle")
}

fn statusline_activity_color(state: &AppState) -> Color {
    if state.runtime_is_active() {
        Color::Cyan
    } else if state.status == "Stream cancelled" {
        Color::Yellow
    } else {
        Color::DarkGray
    }
}

fn statusline_model_label(state: &AppState) -> String {
    let Some(provider_id) = state.selected_provider_id.as_deref() else {
        return String::from("none");
    };
    let Some(model_id) = state.selected_model_id.as_deref() else {
        return String::from("none");
    };
    let model_name = state
        .models_by_provider
        .get(provider_id)
        .and_then(|models| models.iter().find(|model| model.id == model_id))
        .map(|model| model.name.as_str())
        .unwrap_or(model_id);
    format!("{provider_id}/{model_name}")
}

fn statusline_agent_label(state: &AppState) -> String {
    let Some(profile_id) = state.selected_profile_id.as_deref() else {
        return String::from("none");
    };
    state
        .agent_profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .map(|profile| profile.display_name.clone())
        .unwrap_or_else(|| profile_id.to_string())
}

fn statusline_context_label(state: &AppState) -> String {
    format_context_label(
        state
            .context_usage
            .as_ref()
            .map(|usage| usage.used_context_tokens()),
        state
            .context_usage
            .as_ref()
            .and_then(|usage| usage.max_context)
            .or_else(|| selected_model_max_context(state)),
    )
}

fn selected_model_max_context(state: &AppState) -> Option<usize> {
    let provider_id = state.selected_provider_id.as_deref()?;
    let model_id = state.selected_model_id.as_deref()?;
    state
        .models_by_provider
        .get(provider_id)?
        .iter()
        .find(|model| model.id == model_id)
        .and_then(|model| model.max_context)
}

pub(crate) fn format_token_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index != 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_context_label(used_context_tokens: Option<usize>, max_context: Option<usize>) -> String {
    let used_context_tokens = used_context_tokens.unwrap_or_default();
    let used = format_token_count(used_context_tokens);
    match max_context {
        Some(max_context) if max_context > 0 => format!(
            "ctx {used}/{} ({}%)",
            format_token_count(max_context),
            used_context_tokens
                .saturating_mul(100)
                .checked_div(max_context)
                .unwrap_or_default()
        ),
        _ => format!("ctx {used}"),
    }
}
