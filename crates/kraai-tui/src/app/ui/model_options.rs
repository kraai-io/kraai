use kraai_types::ModelOptionKind;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Clear, Paragraph, Widget},
};

use super::super::AppState;
use super::super::model_options::model_option_choices;
use super::{centered_rect, menu_block};

pub(super) fn render(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup = centered_rect(
        area.width.saturating_mul(3) / 4,
        area.height.saturating_mul(3) / 4,
        area,
    );
    let options = state.active_model_options();
    let mut lines = vec![Line::raw("Enter to choose an option; Esc to close")];
    let mut selected_line = 0;
    if options.is_empty() {
        lines.push(Line::raw("This model exposes no configurable options"));
    }
    for (index, option) in options.iter().enumerate() {
        if index == state.option_menu_index {
            selected_line = lines.len();
        }
        let value = state
            .selected_model_options
            .get(&option.id)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                if option.required {
                    String::from("choose a value")
                } else {
                    String::from("unset")
                }
            });
        lines.push(Line::styled(
            format!(
                "{} {} [{}]: {value}",
                if index == state.option_menu_index {
                    ">"
                } else {
                    " "
                },
                option.label,
                option.id
            ),
            if index == state.option_menu_index {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            },
        ));
        if let Some(description) = &option.description {
            lines.push(Line::raw(format!("  {description}")));
        }
    }
    if state.option_editing
        && let Some(option) = options.get(state.option_menu_index)
    {
        lines.push(Line::raw(""));
        if let ModelOptionKind::Integer { min, max } = &option.kind {
            let bounds = match (min, max) {
                (Some(min), Some(max)) => format!("  {min} to {max}"),
                (Some(min), None) => format!("  at least {min}"),
                (None, Some(max)) => format!("  at most {max}"),
                (None, None) => String::new(),
            };
            if !option.required {
                lines.push(Line::raw("Leave empty to unset"));
            }
            selected_line = lines.len();
            lines.push(Line::raw(format!(
                "Enter an integer: {}{bounds}",
                state.option_editor_input
            )));
        }
        for (index, (_, label)) in model_option_choices(option).iter().enumerate() {
            if index == state.option_choice_index {
                selected_line = lines.len();
            }
            lines.push(Line::styled(
                format!(
                    "{} {label}",
                    if index == state.option_choice_index {
                        ">"
                    } else {
                        " "
                    }
                ),
                if index == state.option_choice_index {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                },
            ));
        }
    }
    let scroll = selected_line
        .saturating_add(1)
        .saturating_sub(usize::from(popup.height.saturating_sub(2)));
    Clear.render(popup, buf);
    Paragraph::new(lines)
        .block(menu_block().title("Model options /option <id> <value|--clear>"))
        .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
        .render(popup, buf);
}
