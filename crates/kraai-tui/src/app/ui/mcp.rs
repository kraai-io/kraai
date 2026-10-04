use kraai_runtime::McpAuthState;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Clear, Paragraph, Widget},
};

use super::super::{mcp_auth::status_label, state::AppState};
use crate::components::{ChatHistory, normalize_terminal_text};

pub(super) fn render_mcp(state: &AppState, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);
    let block = super::menu_block().title("MCP servers · ↑/↓ scroll · Esc close");
    let inner = block.inner(area);
    block.render(area, buf);
    let mut text = String::from(
        "Close this view to run commands:\n/mcp login <server> · /mcp cancel <server>\n/mcp logout <server> · /mcp copy <server>\n\n",
    );
    if state.mcp_auth.is_empty() {
        text.push_str("No MCP servers configured\n");
    }
    for status in state.mcp_auth.values() {
        text.push_str(&format!("{}: {}\n", status.server, status_label(status)));
        if let McpAuthState::Pending { auth_url } = &status.state {
            text.push_str(auth_url);
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(&state.status);
    let lines = ChatHistory::wrap_with_prefix(
        &normalize_terminal_text(&text),
        usize::from(inner.width),
        "",
        "",
    );
    let offset = state
        .mcp_scroll
        .get()
        .min(lines.len().saturating_sub(usize::from(inner.height)));
    state.mcp_scroll.set(offset);
    let visible = lines
        .iter()
        .skip(offset)
        .take(usize::from(inner.height))
        .map(|line| Line::raw(line.as_str()))
        .collect::<Vec<_>>();
    Paragraph::new(visible).render(inner, buf);
}
