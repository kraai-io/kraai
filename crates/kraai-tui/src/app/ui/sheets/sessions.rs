use kraai_runtime::Session;

use super::super::super::AppState;
use super::super::sheet::{Sheet, SheetItem, SheetRow, Tone};
use super::{age, notice, plural};

pub(super) fn sessions(state: &AppState) -> Sheet {
    let sessions = state.listed_sessions();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let offset = usize::from(state.offers_new_chat());
    let highlighted = state
        .sheet_index
        .min((sessions.len() + offset).saturating_sub(1));
    let mut rows = Vec::with_capacity(sessions.len() + 3);
    if state.offers_new_chat() {
        rows.push(SheetRow::Item(SheetItem {
            label: String::from("New chat"),
            ..SheetItem::default()
        }));
    }
    let local = sessions
        .iter()
        .filter(|session| state.in_this_workspace(session))
        .count();
    for (index, session) in sessions.iter().enumerate() {
        if index == 0 && local > 0 {
            rows.push(SheetRow::Heading(short_path(
                &state.workspace_dir,
                &state.home_dir,
            )));
        } else if index == local && local > 0 {
            rows.push(SheetRow::Heading(String::from("elsewhere")));
        }
        let armed = state.sheet_armed && index + offset == highlighted;
        rows.push(SheetRow::Item(row(
            state,
            session,
            index < local,
            armed,
            now,
        )));
    }
    Sheet {
        caption: String::from("sessions"),
        count: plural(sessions.len(), "session"),
        rows,
        selected: state.sheet_index,
        empty: String::from("No sessions match"),
        notes: notice(state),
        align_choices: true,
    }
}

fn row(state: &AppState, session: &Session, local: bool, armed: bool, now: u64) -> SheetItem {
    let (tag, tone) = if armed {
        ("del again to delete", Tone::Danger)
    } else if session.waiting_for_approval {
        ("needs approval", Tone::Waiting)
    } else if session.is_running {
        ("running", Tone::Plain)
    } else if state.current_session_id.as_ref() == Some(&session.id) {
        ("current", Tone::Plain)
    } else {
        ("", Tone::Plain)
    };
    SheetItem {
        label: session.title.clone().unwrap_or_else(|| {
            format!(
                "Session {}",
                session.id.as_str().chars().take(8).collect::<String>()
            )
        }),
        detail: if local {
            String::new()
        } else {
            short_path(&session.workspace_dir, &state.home_dir)
        },
        meta: age(now.saturating_sub(session.updated_at)),
        tag: tag.to_owned(),
        tone,
        ..SheetItem::default()
    }
}

/// Writes a path under the home directory with `~` in place of the home directory.
fn short_path(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::short_path;

    #[test]
    fn only_paths_under_home_are_shortened() {
        assert_eq!(short_path("/home/me/code", "/home/me"), "~/code");
        assert_eq!(short_path("/home/me", "/home/me"), "~");
        assert_eq!(short_path("/home/melon", "/home/me"), "/home/melon");
        assert_eq!(short_path("/tmp/x", "/home/me"), "/tmp/x");
        assert_eq!(short_path("/tmp/x", ""), "/tmp/x");
    }
}
