use super::*;

pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    /// The command opens a list and takes no arguments, so Space after its name runs it.
    pub(super) opens_list: bool,
    /// The command list shows the command only while an error is waiting to be read.
    pub(super) needs_error: bool,
}

const fn command(name: &'static str, description: &'static str) -> SlashCommand {
    SlashCommand {
        name,
        description,
        opens_list: false,
        needs_error: false,
    }
}

const fn list(name: &'static str, description: &'static str) -> SlashCommand {
    SlashCommand {
        opens_list: true,
        ..command(name, description)
    }
}

pub(super) static SLASH_COMMANDS: [SlashCommand; 9] = [
    list("model", "Switch model and change its settings"),
    list("agent", "Switch agent"),
    command("new", "Start a new chat"),
    list("sessions", "Open or delete sessions"),
    command("undo", "Restore the last message"),
    list("providers", "Connect and edit providers"),
    command("mcp", "MCP servers and sign-in"),
    SlashCommand {
        needs_error: true,
        ..command("error", "Read the last error")
    },
    command("quit", "Exit"),
];

pub(super) fn active_command_prefix(input: &str) -> Option<&str> {
    let cmd = input.strip_prefix('/')?;
    if cmd.chars().any(char::is_whitespace) {
        return None;
    }
    Some(cmd)
}

pub(super) fn is_known_slash_command(command_line: &str) -> bool {
    command_line
        .split_whitespace()
        .next()
        .is_some_and(|name| SLASH_COMMANDS.iter().any(|command| command.name == name))
}

/// Commands whose name contains `query`, with names that start with it first.
pub(super) fn slash_command_matches(query: &str) -> Vec<&'static SlashCommand> {
    let mut matches: Vec<_> = SLASH_COMMANDS
        .iter()
        .filter(|command| command.name.contains(query))
        .collect();
    matches.sort_by_key(|command| !command.name.starts_with(query));
    matches
}

impl AppState {
    pub(super) fn command_matches(&self) -> Vec<&'static SlashCommand> {
        if self.mode != UiMode::Chat
            || self.command_popup_dismissed
            || self.has_local_script_approval()
            || self.draft_images.len() != 0
        {
            return Vec::new();
        }
        let mut matches =
            active_command_prefix(self.command_filter.as_deref().unwrap_or(&self.input))
                .map(slash_command_matches)
                .unwrap_or_default();
        matches.retain(|command| !command.needs_error || self.last_error.is_some());
        matches
    }
}

impl App {
    pub(super) fn command_popup_visible(&self) -> bool {
        !self.state.command_matches().is_empty()
    }

    pub(super) fn reset_command_selection(&mut self) {
        self.state.command_filter = None;
        self.state.sheet_index = 0;
        self.state.sheet_scroll.set(0);
    }

    /// Handles keys that act on the command list; false leaves the key to the composer.
    pub(super) fn handle_command_list_key(&mut self, key: KeyEvent) -> bool {
        let matches = self.state.command_matches();
        let len = matches.len();
        let index = self.state.sheet_index.min(len.saturating_sub(1));
        let Some(selected) = matches.get(index).copied() else {
            return false;
        };
        let written = self.state.input.strip_prefix('/') == Some(selected.name);
        let step = match key.code {
            KeyCode::Tab if !written => Some(index),
            KeyCode::Down | KeyCode::Tab => Some(model_menu_next_index(index, len)),
            KeyCode::Up | KeyCode::BackTab => Some(model_menu_previous_index(index, len)),
            _ => None,
        };
        if let Some(index) = step {
            if let Some(command) = matches.get(index) {
                self.walk_command_list(index, command.name);
            }
            return true;
        }
        match key.code {
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.run_listed_command(selected.name)
            }
            KeyCode::Char(' ')
                if written
                    && selected.opens_list
                    && self.state.input_cursor == self.state.input.len() =>
            {
                self.run_listed_command(selected.name)
            }
            _ => return false,
        }
        true
    }

    /// Highlights a row and writes its command into the composer. The list keeps showing what
    /// the typed text matched, so repeated presses walk every match.
    fn walk_command_list(&mut self, index: usize, name: &str) {
        let filter = self
            .state
            .command_filter
            .take()
            .unwrap_or_else(|| self.state.input.clone());
        self.set_input_text(format!("/{name}"));
        self.state.command_filter = Some(filter);
        self.state.sheet_index = index;
    }

    fn run_listed_command(&mut self, name: &str) {
        self.clear_message_draft();
        self.state.command_popup_dismissed = false;
        self.reset_command_selection();
        self.handle_command(name);
    }
}
