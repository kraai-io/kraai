use super::*;

impl App {
    pub(super) fn handle_composer_shortcut(&mut self, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('e') if control => self.state.editor_requested = true,
            KeyCode::Left if control => {
                self.state.input_cursor = word_left(&self.state.input, self.state.input_cursor)
            }
            KeyCode::Right if control => {
                self.state.input_cursor = word_right(&self.state.input, self.state.input_cursor)
            }
            KeyCode::Char('b') if alt => {
                self.state.input_cursor = word_left(&self.state.input, self.state.input_cursor)
            }
            KeyCode::Char('f') if alt => {
                self.state.input_cursor = word_right(&self.state.input, self.state.input_cursor)
            }
            KeyCode::Backspace if control || alt => self.delete_input_word(false),
            KeyCode::Char('w') if control => self.delete_input_word(false),
            KeyCode::Delete if control => self.delete_input_word(true),
            KeyCode::Char('d') if alt => self.delete_input_word(true),
            _ => return false,
        }
        self.reset_completion_cycle();
        true
    }

    fn delete_input_word(&mut self, forward: bool) {
        self.reset_input_history_navigation();
        let cursor = self.state.input_cursor;
        let boundary = if forward {
            word_right(&self.state.input, cursor)
        } else {
            word_left(&self.state.input, cursor)
        };
        self.state
            .input
            .drain(cursor.min(boundary)..cursor.max(boundary));
        self.state.input_cursor = cursor.min(boundary);
    }

    pub(super) fn open_composer_editor(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
    ) -> Result<()> {
        use ratatui::crossterm::{
            event::{
                DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
                EnableMouseCapture,
            },
            execute,
            terminal::{
                EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            },
        };
        self.state.editor_requested = false;
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            });
        let Some(editor) = editor else {
            self.state.status = String::from("Set VISUAL or EDITOR to edit the prompt externally");
            return Ok(());
        };
        let original_input = self.state.input.clone();
        let original_session = self.state.current_session_id.clone();
        let mut file = tempfile::Builder::new()
            .prefix("kraai-prompt-")
            .suffix(".txt")
            .tempfile()?;
        file.write_all(self.state.input.as_bytes())?;
        file.flush()?;
        #[cfg(unix)]
        let _signal_guard = EditorSignalGuard::new()?;
        disable_raw_mode()?;
        let edit_result = (|| -> Result<()> {
            execute!(
                io::stdout(),
                DisableMouseCapture,
                DisableBracketedPaste,
                LeaveAlternateScreen
            )?;
            run_editor(&editor, file.path(), || {
                self.process_events();
            })
        })();
        let restore_result = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        );
        let raw_result = enable_raw_mode();
        if let Err(error) = restore_result
            .and(raw_result)
            .and_then(|()| terminal.clear())
        {
            self.state.exit = true;
            let (_, path) = file.keep()?;
            return Err(color_eyre::eyre::eyre!(
                "Terminal restore failed: {error}; draft preserved at {}",
                path.display()
            ));
        }
        let edited = edit_result.and_then(|()| {
            let text = std::fs::read_to_string(file.path())?;
            self.apply_edited_prompt(text, original_session.as_deref(), &original_input)
        });
        match edited {
            Ok(()) => {
                self.state.status = String::from("Prompt updated from editor");
            }
            Err(error) => {
                let (_, path) = file.keep()?;
                self.set_error(format!("{error}; draft preserved at {}", path.display()));
                if self.state.exit {
                    return Err(color_eyre::eyre::eyre!(self.state.status.clone()));
                }
            }
        }
        Ok(())
    }

    pub(super) fn apply_edited_prompt(
        &mut self,
        text: String,
        original_session: Option<&str>,
        original_input: &str,
    ) -> Result<()> {
        if self.state.exit
            || self.state.current_session_id.as_deref() != original_session
            || self.state.input != original_input
        {
            return Err(color_eyre::eyre::eyre!(
                "Session or prompt changed, or Kraai is exiting"
            ));
        }
        self.set_input_text(text);
        Ok(())
    }
}

#[cfg(unix)]
struct EditorSignalGuard(&'static std::sync::atomic::AtomicBool);

#[cfg(unix)]
impl EditorSignalGuard {
    fn new() -> Result<Self> {
        use std::sync::{
            Arc, OnceLock,
            atomic::{AtomicBool, Ordering},
        };
        static DEFAULT_ENABLED: OnceLock<std::io::Result<Arc<AtomicBool>>> = OnceLock::new();
        let enabled = DEFAULT_ENABLED
            .get_or_init(|| {
                let enabled = Arc::new(AtomicBool::new(true));
                for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGQUIT] {
                    signal_hook::flag::register_conditional_default(signal, enabled.clone())?;
                }
                Ok(enabled)
            })
            .as_ref()
            .map_err(|error| color_eyre::eyre::eyre!("Cannot handle editor signals: {error}"))?;
        enabled.store(false, Ordering::SeqCst);
        Ok(Self(enabled))
    }
}

#[cfg(unix)]
impl Drop for EditorSignalGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn word_left(input: &str, cursor: usize) -> usize {
    let mut position = cursor;
    let mut word = false;
    for (index, ch) in input.get(..cursor).unwrap_or_default().char_indices().rev() {
        if word && ch.is_whitespace() {
            break;
        }
        word |= !ch.is_whitespace();
        position = index;
    }
    position
}

fn word_right(input: &str, cursor: usize) -> usize {
    let mut word = false;
    for (index, ch) in input.get(cursor..).unwrap_or_default().char_indices() {
        if word && ch.is_whitespace() {
            return cursor + index;
        }
        word |= !ch.is_whitespace();
    }
    input.len()
}

fn run_editor(editor: &str, path: &std::path::Path, mut tick: impl FnMut()) -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("kraai-editor")
        .arg(path)
        .spawn()?;
    loop {
        tick();
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(color_eyre::eyre::eyre!("Editor exited with {status}"));
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::run_editor;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions follow fallible fixture setup"
    )]
    fn editor_command_supports_arguments_and_quoted_paths() -> color_eyre::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("draft with spaces ' $dollar.txt");
        std::fs::write(&path, "original")?;
        run_editor("sh -c 'printf edited > \"$1\"' editor", &path, || {})?;
        assert_eq!(std::fs::read_to_string(&path)?, "edited");
        assert!(run_editor("sh -c 'exit 7' editor", &path, || {}).is_err());
        assert_eq!(std::fs::read_to_string(&path)?, "edited");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "subprocess assertions verify signal handling"
    )]
    fn editor_signal_guard_protects_parent_and_restores_default() -> color_eyre::Result<()> {
        use std::io::Write;
        use std::os::unix::process::ExitStatusExt;
        const CHILD_ENV: &str = "KRAAI_EDITOR_SIGNAL_TEST";
        if std::env::var_os(CHILD_ENV).is_some() {
            for _ in 0..2 {
                let guard = super::EditorSignalGuard::new()?;
                signal_hook::low_level::raise(signal_hook::consts::SIGINT)?;
                signal_hook::low_level::raise(signal_hook::consts::SIGQUIT)?;
                let child = std::process::Command::new("sh")
                    .args(["-c", "kill -INT $$"])
                    .status()?;
                assert_eq!(child.signal(), Some(signal_hook::consts::SIGINT));
                drop(guard);
            }
            std::io::stdout().write_all(b"editor survived interrupts\n")?;
            std::io::stdout().flush()?;
            signal_hook::low_level::raise(signal_hook::consts::SIGINT)?;
            return Ok(());
        }
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "app::composer::tests::editor_signal_guard_protects_parent_and_restores_default",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .output()?;
        assert!(String::from_utf8_lossy(&output.stdout).contains("editor survived interrupts"));
        assert_eq!(output.status.signal(), Some(signal_hook::consts::SIGINT));
        Ok(())
    }
}
