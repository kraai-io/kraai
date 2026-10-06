use kraai_runtime::McpAuthState;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Clear, Paragraph, Widget},
};

use super::super::{mcp_auth::status_label, state::AppState};
use crate::components::{ChatHistory, normalize_terminal_text};

pub(super) fn render_mcp(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup = super::centered_rect(
        area.width.saturating_mul(11) / 12,
        area.height.saturating_mul(4) / 5,
        area,
    );
    Clear.render(popup, buf);
    let block = super::menu_block().title("/mcp");
    let inner = block.inner(popup);
    block.render(popup, buf);
    let list_height = u16::try_from(state.mcp_auth.len())
        .unwrap_or(u16::MAX)
        .max(1)
        .min(inner.height / 3);
    let [header, list, details, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(list_height),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .areas(inner);
    Paragraph::new("Servers · ↑/↓ select · r refresh auth · Esc close").render(header, buf);
    let selected = state
        .mcp_selected
        .as_ref()
        .and_then(|name| state.mcp_auth.get(name))
        .or_else(|| state.mcp_auth.values().next());
    let index = selected
        .and_then(|selected| {
            state
                .mcp_auth
                .keys()
                .position(|name| name == &selected.server)
        })
        .unwrap_or(0);
    let offset = super::menu_scroll_offset(index, state.mcp_auth.len(), usize::from(list.height));
    let rows: Vec<_> = state
        .mcp_auth
        .values()
        .enumerate()
        .skip(offset)
        .take(usize::from(list.height))
        .map(|(i, status)| {
            Line::styled(
                format!(
                    "{} {}: {}{}",
                    if i == index { ">" } else { " " },
                    status.server,
                    status_label(status),
                    if status.error.is_some() {
                        " · error"
                    } else {
                        ""
                    }
                ),
                super::selection_style(i == index),
            )
        })
        .collect();
    Paragraph::new(rows).render(list, buf);
    let mut text = String::new();
    let actions = if let Some(status) = selected {
        let actions = match &status.state {
            McpAuthState::Unavailable => "r refresh auth · Esc close",
            McpAuthState::SignedOut => "Enter / b sign in · r refresh auth · Esc close",
            McpAuthState::Starting => "x cancel sign-in · Esc close",
            McpAuthState::Pending { auth_url } => {
                text.push_str(&format!(
                    "Complete sign-in in your browser.\n\nSign-in URL\n{auth_url}\n"
                ));
                "Enter / o open browser · y copy URL · x cancel"
            }
            McpAuthState::Authenticated => "l log out · r refresh auth · Esc close",
        };
        if let Some(error) = &status.error {
            text.push_str(&format!("\nError: {error}\n"));
        }
        if let Some(message) = state.mcp_feedback.get(&status.server) {
            text.push_str(&format!("\n{message}\n"));
        }
        actions
    } else {
        text.push_str(if state.mcp_loaded {
            "No MCP servers configured.\nAdd servers to mcp.toml and restart Kraai.\n"
        } else {
            "Loading MCP servers...\n"
        });
        "r refresh auth · Esc close"
    };
    if let Some(error) = &state.mcp_error {
        text.push_str(&format!("\n{error}\n"));
    }
    let detail_inner = details;
    let lines = ChatHistory::wrap_with_prefix(
        &normalize_terminal_text(&text),
        usize::from(detail_inner.width),
        "",
        "",
    );
    let offset = state
        .mcp_scroll
        .get()
        .min(lines.len().saturating_sub(usize::from(detail_inner.height)));
    state.mcp_scroll.set(offset);
    let visible: Vec<_> = lines
        .iter()
        .skip(offset)
        .take(usize::from(detail_inner.height))
        .map(|line| Line::raw(line.as_str()))
        .collect();
    Paragraph::new(visible).render(detail_inner, buf);
    Paragraph::new(actions)
        .style(Style::default().fg(Color::DarkGray))
        .render(footer, buf);
}
