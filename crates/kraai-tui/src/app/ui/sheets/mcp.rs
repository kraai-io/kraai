use kraai_runtime::{McpAuthState, McpAuthStatus};

use super::super::super::{AppState, mcp_auth::status_label};
use super::super::sheet::{Note, Sheet, SheetItem, SheetRow, Tone};
use super::{notice, plural};

pub(super) fn mcp(state: &AppState) -> Sheet {
    match &state.mcp_open {
        Some(server) => server_sheet(state, server),
        None => servers(state),
    }
}

fn servers(state: &AppState) -> Sheet {
    let servers = state.listed_mcp_servers();
    Sheet {
        caption: String::from("mcp"),
        count: plural(servers.len(), "server"),
        rows: servers
            .iter()
            .map(|status| SheetRow::Item(server_row(status)))
            .collect(),
        selected: state.sheet_index,
        empty: String::from(if !state.mcp_loaded {
            "Loading MCP servers"
        } else if state.mcp_auth.is_empty() {
            "No MCP servers configured. Add servers to mcp.toml and restart Kraai"
        } else {
            "No servers match"
        }),
        notes: closing_notes(state),
        align_choices: true,
    }
}

fn server_row(status: &McpAuthStatus) -> SheetItem {
    let (tag, tone) = if status.error.is_some() {
        ("error", Tone::Danger)
    } else {
        match status.state {
            McpAuthState::Authenticated => ("signed in", Tone::Plain),
            McpAuthState::Starting | McpAuthState::Pending { .. } => {
                (status_label(status), Tone::Waiting)
            }
            McpAuthState::Unavailable | McpAuthState::SignedOut => ("", Tone::Plain),
        }
    };
    SheetItem {
        label: status.server.clone(),
        detail: if tag.is_empty() {
            status_label(status).to_owned()
        } else {
            String::new()
        },
        tag: tag.to_owned(),
        tone,
        ..SheetItem::default()
    }
}

fn server_sheet(state: &AppState, server: &str) -> Sheet {
    let status = state.mcp_auth.get(server);
    let actions = state.listed_mcp_actions();
    let mut notes = Vec::new();
    if let Some(status) = status {
        if let McpAuthState::Pending { auth_url } = &status.state {
            notes.push(Note::new("Complete sign-in in your browser.", Tone::Plain));
            notes.push(Note::field("url", auth_url));
        }
        if let Some(error) = &status.error {
            notes.push(Note::new(error, Tone::Danger));
        }
    }
    if let Some(message) = state.mcp_feedback.get(server) {
        notes.push(Note::new(message, Tone::Plain));
    }
    notes.extend(closing_notes(state));
    Sheet {
        caption: server.to_owned(),
        count: status.map(status_label).unwrap_or_default().to_owned(),
        rows: actions
            .iter()
            .map(|action| {
                SheetRow::Item(SheetItem {
                    label: action.label().to_owned(),
                    ..SheetItem::default()
                })
            })
            .collect(),
        selected: state.sheet_index,
        empty: match status {
            None => format!("No MCP server named {server}"),
            Some(status) if matches!(status.state, McpAuthState::Unavailable) => {
                String::from("This server does not sign in through a browser")
            }
            Some(_) => String::from("No actions match"),
        },
        notes,
        align_choices: true,
    }
}

fn closing_notes(state: &AppState) -> Vec<Note> {
    state
        .mcp_error
        .iter()
        .map(|error| Note::new(error, Tone::Danger))
        .chain(notice(state))
        .collect()
}
