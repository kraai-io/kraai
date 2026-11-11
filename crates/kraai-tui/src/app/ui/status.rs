use std::time::{Duration, Instant};

use crate::duration::format_duration;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::super::model_options::option_summary;
use super::super::{AppState, ScriptPhase};
use crate::components::{display_width, fitting_prefix, normalize_terminal_text};
use crate::theme;

const GAP: usize = 2;

/// One piece of the left side of the status line.
struct Fact {
    text: String,
    style: Style,
    /// When the line is too narrow, facts go in this order, lowest first. `None` always stays.
    drop: Option<u8>,
}

impl Fact {
    fn new(text: impl Into<String>, drop: Option<u8>) -> Self {
        Self {
            text: text.into(),
            style: Style::default().fg(theme::MUTED),
            drop,
        }
    }
}

/// Draws the footer from the bottom up: the status line, an error waiting to be read above it,
/// and the open sheet's key hints above that, so they touch the sheet. The status line is what
/// the turn is doing, then what the composer talks to.
pub(super) fn render_status(state: &AppState, area: Rect, buf: &mut Buffer) {
    let inset = u16::from(area.width > 2);
    let area = Rect::new(area.x + inset, area.y, area.width - 2 * inset, area.height);
    let mut rows = (area.y..area.bottom())
        .rev()
        .map(|y| Rect::new(area.x, y, area.width, 1));
    if (state.last_error.is_none() || area.height > 1)
        && let Some(row) = rows.next()
    {
        render_row(facts(state), None, row, buf);
    }
    let danger = Style::default().fg(theme::DANGER);
    if let Some(error) = &state.last_error
        && let Some(row) = rows.next()
    {
        render_row(
            vec![Fact {
                text: error.lines().next().unwrap_or("Error").to_owned(),
                style: danger,
                drop: None,
            }],
            Some(("/error", danger)),
            row,
            buf,
        );
    }
    if let Some(hints) = super::sheets::hints(state)
        && let Some(row) = rows.next()
    {
        render_row(vec![Fact::new(hints, None)], None, row, buf);
    }
}

/// Draws `facts` from the left edge and `tag` against the right one. The tag keeps its room and
/// the facts give way, least important first.
fn render_row(mut facts: Vec<Fact>, tag: Option<(&str, Style)>, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let (tag, tag_style) = tag.unwrap_or_default();
    let tag = fit(tag, width);
    let tag_width = display_width(&tag);
    let room = if tag.is_empty() {
        width
    } else {
        width.saturating_sub(tag_width + GAP)
    };
    facts.retain(|fact| !fact.text.is_empty());
    let needed = |facts: &[Fact]| {
        facts
            .iter()
            .map(|fact| display_width(&normalize_terminal_text(&fact.text)))
            .sum::<usize>()
            + GAP * facts.len().saturating_sub(1)
    };
    while needed(&facts) > room {
        let Some(index) = facts
            .iter()
            .enumerate()
            .filter_map(|(index, fact)| Some((fact.drop?, index)))
            .min()
            .map(|(_, index)| index)
        else {
            break;
        };
        facts.remove(index);
    }
    let mut x = 0;
    for fact in &facts {
        let text = fit(&fact.text, room.saturating_sub(x));
        if text.is_empty() {
            break;
        }
        put(buf, area, x, &text, fact.style);
        x += display_width(&text) + GAP;
    }
    put(buf, area, width - tag_width, &tag, tag_style);
}

fn put(buf: &mut Buffer, area: Rect, column: usize, text: &str, style: Style) {
    if let Ok(column) = u16::try_from(column)
        && column < area.width
        && area.height > 0
    {
        buf.set_string(area.x + column, area.y, text, style);
    }
}

pub(super) fn fit(text: &str, width: usize) -> String {
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

/// What the turn is doing, in the colour that says how much it wants attention. Empty while
/// there is nothing to report.
fn activity(state: &AppState) -> Fact {
    let (text, style) = activity_text(state, Instant::now());
    Fact {
        text,
        style,
        drop: None,
    }
}

/// The words and style of what the turn is doing at `now`. A busy turn breathes its colour in
/// time with the running scripts' dots.
pub(in crate::app) fn activity_text(state: &AppState, now: Instant) -> (String, Style) {
    let accent = Style::default().fg(theme::ACCENT);
    let waiting = Style::default().fg(theme::WAITING);
    let muted = Style::default().fg(theme::MUTED);
    if state.script_decision_pending() {
        return (String::from("Submitting decision"), accent);
    }
    if state.script_phase == ScriptPhase::AwaitingApproval {
        return (String::from("Needs approval"), waiting);
    }
    if state.runtime_is_active() {
        let activity = if state.retry_waiting {
            "Retrying"
        } else if state.script_phase == ScriptPhase::Executing {
            "Running"
        } else {
            "Working"
        };
        let text = match state.turn_timer.elapsed(now) {
            Some(elapsed) => format!("{activity} {}", whole_seconds(elapsed)),
            None => activity.to_owned(),
        };
        let colour = if state.retry_waiting {
            theme::WAITING
        } else {
            theme::ACCENT
        };
        let colour = state
            .palette
            .muted
            .and_then(|muted| theme::breathe(state.breath(now), colour, muted))
            .unwrap_or(colour);
        return (text, Style::default().fg(colour));
    }
    let Some(took) = state.turn_timer.last_duration().map(whole_seconds) else {
        return if state.turn_cancelled {
            (String::from("Cancelled"), waiting)
        } else {
            (String::new(), muted)
        };
    };
    if state.turn_cancelled {
        (format!("Cancelled after {took}"), waiting)
    } else if state.last_error.is_some() {
        (format!("Stopped after {took}"), muted)
    } else {
        let colour = if state.feedback.completion_until.is_some() {
            state.feedback.completion_color(now, state.palette.muted)
        } else {
            theme::MUTED
        };
        (format!("Finished in {took}"), Style::default().fg(colour))
    }
}

fn whole_seconds(duration: Duration) -> String {
    format_duration(Duration::from_secs(duration.as_secs()))
}

/// The status line: what the turn is doing, the model, how it is set up, the agent, then what
/// the session has used.
fn facts(state: &AppState) -> Vec<Fact> {
    let (model, options) = model_facts(state);
    let (tokens, percent) = context_facts(state);
    let images = match state.draft_images.len() {
        0 => String::new(),
        1 => String::from("1 image"),
        count => format!("{count} images"),
    };
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
    vec![
        activity(state),
        model,
        Fact::new(options, Some(3)),
        Fact::new(agent_label(state), Some(1)),
        Fact::new(images, Some(2)),
        Fact::new(tokens, percent.as_ref().map(|_| 0)),
        Fact::new(percent.unwrap_or_default(), None),
        Fact::new(cost, None),
    ]
}

/// The model's name and its settings. A missing model takes the colour of something waiting.
fn model_facts(state: &AppState) -> (Fact, String) {
    let (Some(provider_id), Some(model_id)) = (
        state.selected_provider_id.as_deref(),
        state.selected_model_id.as_deref(),
    ) else {
        return (
            Fact {
                text: String::from("No model selected"),
                style: Style::default().fg(theme::WAITING),
                drop: None,
            },
            String::new(),
        );
    };
    let (name, options) = match state.find_model(provider_id, model_id) {
        Some(model) => (
            model.name.clone(),
            option_summary(model, &state.selected_model_options),
        ),
        None => (model_id.to_owned(), String::new()),
    };
    (Fact::new(name, None), options)
}

fn agent_label(state: &AppState) -> String {
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

/// The tokens in the context and, when the model's limit is known, how full that makes it.
fn context_facts(state: &AppState) -> (String, Option<String>) {
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
    format_context(
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

fn format_context(
    used_context_tokens: Option<usize>,
    max_context: Option<usize>,
) -> (String, Option<String>) {
    let used_context_tokens = used_context_tokens.unwrap_or_default();
    let used = format_token_count(used_context_tokens);
    match max_context {
        Some(max_context) if max_context > 0 => (
            format!("{used}/{}", format_token_count(max_context)),
            Some(format!(
                "{}%",
                used_context_tokens
                    .saturating_mul(100)
                    .checked_div(max_context)
                    .unwrap_or_default()
            )),
        ),
        _ => (format!("{used} tokens"), None),
    }
}

#[cfg(test)]
mod tests;
