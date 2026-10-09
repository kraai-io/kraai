use std::time::{Duration, Instant};

use super::super::duration::format_duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::super::{AppState, ScriptPhase};
use super::STATUSLINE_STREAMING_FRAMES;
use crate::components::{display_width, fitting_prefix, normalize_terminal_text};

pub(super) fn render_status(state: &AppState, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let padding = area
        .height
        .saturating_sub(1 + u16::from(state.last_error.is_some()))
        .min(1);
    let area = Rect::new(
        area.x + u16::from(area.width > 2),
        area.y + padding,
        area.width.saturating_sub(2 * u16::from(area.width > 2)),
        area.height - padding,
    );
    let activity = statusline_activity_label(state);
    let context = statusline_context_label(state);
    let incomplete = state.cost_recovery_list_pending
        || state
            .current_session_id
            .as_ref()
            .is_some_and(|id| state.cost_recovery_sessions.contains(id));
    let cost = format!(
        "{}{}",
        state.session_cost,
        if incomplete { " (incomplete)" } else { "" }
    );
    let full = format!("{context} · {cost}");
    let compact_context = context
        .split_once('(')
        .map(|(_, percentage)| percentage.trim_end_matches(')'))
        .unwrap_or(&context);
    let compact = format!("{compact_context} · {cost}");
    let mut metadata = statusline_model_label(state);
    let agent = statusline_agent_label(state);
    if !agent.is_empty() {
        metadata.push_str(&format!(" · {agent}"));
    }
    let count = state.draft_images.len();
    if count > 0 {
        metadata.push_str(&format!(
            " · {count} {}",
            if count == 1 { "image" } else { "images" }
        ));
    }
    let usage = if display_width(&activity) + 6 + display_width(&metadata) + display_width(&full)
        <= usize::from(area.width)
    {
        full
    } else {
        compact
    };
    if let Some(error) = &state.last_error {
        let preview = format!("F8 error · {}", error.lines().next().unwrap_or("Error"));
        Paragraph::new(fit(&preview, usize::from(area.width)))
            .style(Style::default().fg(Color::Red))
            .render(Rect::new(area.x, area.y, area.width, 1), buf);
        if area.height < 2 {
            return;
        }
    }
    let details = if state.mode == super::super::UiMode::Executions {
        String::from("↑/↓ select · Enter toggle · Esc close")
    } else {
        format!("{metadata} · {usage}")
    };
    render_pair(
        &activity,
        &details,
        Style::default().fg(statusline_activity_color(state)),
        Style::default().fg(Color::DarkGray),
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
        buf,
    );
}

fn render_pair(
    left: &str,
    right: &str,
    left_style: Style,
    right_style: Style,
    area: Rect,
    buf: &mut Buffer,
) {
    let width = usize::from(area.width);
    let right_width = display_width(&normalize_terminal_text(right))
        .min(width.saturating_sub(display_width(left).min(width / 2) + 3));
    let separator = if left.is_empty() || right.is_empty() {
        ""
    } else {
        " · "
    };
    let left_width = width.saturating_sub(right_width + display_width(separator));
    let left = fit(left, left_width);
    let right = fit(right, right_width);
    Paragraph::new(Line::from(vec![
        Span::styled(left, left_style),
        Span::styled(separator, Style::default().fg(Color::DarkGray)),
        Span::styled(right, right_style),
    ]))
    .render(area, buf);
}

fn fit(text: &str, width: usize) -> String {
    let text = normalize_terminal_text(text).replace(['\n', '\r'], " ");
    if display_width(&text) <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let (prefix, _) = fitting_prefix(&text, width - 1);
    let mut result = prefix.to_string();
    result.push('…');
    result
}

fn statusline_activity_label(state: &AppState) -> String {
    if state.script_decision_pending() {
        return String::from("Submitting decision");
    }
    if state.script_phase == ScriptPhase::AwaitingApproval {
        return String::from("Needs approval");
    }
    if state.runtime_is_active() {
        let activity = if state.retry_waiting {
            "Retrying"
        } else if state.script_phase == ScriptPhase::Executing {
            "Running"
        } else {
            "Working"
        };
        let frame_index = state.statusline_animation_frame % STATUSLINE_STREAMING_FRAMES.len();
        let frame = STATUSLINE_STREAMING_FRAMES
            .get(frame_index)
            .unwrap_or(&"running")
            .to_string();
        return state
            .turn_timer
            .elapsed(Instant::now())
            .map(|elapsed| {
                format!(
                    "{frame} {activity} · {}",
                    format_duration(Duration::from_secs(elapsed.as_secs()))
                )
            })
            .unwrap_or_else(|| format!("{frame} {activity}"));
    }
    if state.status == "Stream cancelled" {
        return statusline_terminal_activity_label("Cancelled after", state);
    }
    if state.last_error.is_some() {
        return statusline_terminal_activity_label("Stopped after", state);
    }
    if state.turn_timer.last_duration().is_some() {
        statusline_terminal_activity_label("Finished in", state)
    } else {
        String::from("Ready")
    }
}

fn statusline_terminal_activity_label(label: &str, state: &AppState) -> String {
    state
        .turn_timer
        .last_duration()
        .map(|elapsed| {
            format!(
                "{label} {}",
                format_duration(Duration::from_secs(elapsed.as_secs()))
            )
        })
        .unwrap_or_else(|| label.trim_end_matches(" after").to_string())
}

fn statusline_activity_color(state: &AppState) -> Color {
    if state.script_phase == ScriptPhase::AwaitingApproval
        || state.retry_waiting
        || state.status == "Stream cancelled"
    {
        Color::Yellow
    } else if state.feedback.completion_until.is_some()
        && !state.runtime_is_active()
        && state.last_error.is_none()
    {
        state
            .feedback
            .completion_color(Instant::now(), state.palette.muted)
    } else {
        Color::DarkGray
    }
}

fn statusline_model_label(state: &AppState) -> String {
    let Some(provider_id) = state.selected_provider_id.as_deref() else {
        return String::from("No model selected");
    };
    let Some(model_id) = state.selected_model_id.as_deref() else {
        return String::from("No model selected");
    };
    let model_name = state
        .models_by_provider
        .get(provider_id)
        .and_then(|models| models.iter().find(|model| model.id == model_id))
        .map(|model| model.name.as_str())
        .unwrap_or(model_id);
    if state.selected_model_options.is_empty() {
        return model_name.to_owned();
    }
    let options = state
        .selected_model_options
        .iter()
        .map(|(id, value)| format!("{id}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{model_name} ({options})")
}

fn statusline_agent_label(state: &AppState) -> String {
    let Some(profile_id) = state.selected_profile_id.as_deref() else {
        return String::new();
    };
    state
        .agent_profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .map(|profile| profile.display_name.clone())
        .unwrap_or_else(|| profile_id.to_string())
}

fn statusline_context_label(state: &AppState) -> String {
    let max_context = match state.selected_model() {
        Some(model) => model.max_context,
        None => state
            .context_usage
            .as_ref()
            .filter(|usage| {
                state.selected_provider_id.as_deref() == Some(usage.provider_id.as_str())
                    && state.selected_model_id.as_deref() == Some(usage.model_id.as_str())
            })
            .and_then(|usage| usage.max_context),
    };
    format_context_label(
        state
            .context_usage
            .as_ref()
            .map(|usage| usage.used_context_tokens()),
        max_context,
    )
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
            "{used}/{} ({}%)",
            format_token_count(max_context),
            used_context_tokens
                .saturating_mul(100)
                .checked_div(max_context)
                .unwrap_or_default()
        ),
        _ => used,
    }
}

#[cfg(test)]
mod tests;
