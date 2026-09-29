use std::io::{Result, stdout};

use ratatui::crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
};

pub(crate) fn enable() -> Result<()> {
    execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    #[cfg(unix)]
    execute!(
        stdout(),
        ratatui::crossterm::event::PushKeyboardEnhancementFlags(
            ratatui::crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;
    Ok(())
}

pub(crate) fn disable() -> Result<()> {
    disable_with(&mut stdout())
}

fn disable_with(writer: &mut impl std::io::Write) -> Result<()> {
    #[cfg(unix)]
    let keyboard = execute!(
        writer,
        ratatui::crossterm::event::PopKeyboardEnhancementFlags
    );
    #[cfg(not(unix))]
    let keyboard = Ok(());
    let mouse = execute!(writer, DisableMouseCapture);
    let paste = execute!(writer, DisableBracketedPaste);
    keyboard.and(mouse).and(paste)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Error, Write};

    #[test]
    fn cleanup_attempts_remaining_modes_after_a_write_or_flush_failure() {
        struct FailingWriter {
            bytes: Vec<u8>,
            fail_write: bool,
            fail_flush: bool,
        }
        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> Result<usize> {
                if std::mem::take(&mut self.fail_write) {
                    return Err(Error::other("write failed"));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> Result<()> {
                if std::mem::take(&mut self.fail_flush) {
                    return Err(Error::other("flush failed"));
                }
                Ok(())
            }
        }
        for fail_write in [false, true] {
            let mut writer = FailingWriter {
                bytes: Vec::new(),
                fail_write,
                fail_flush: !fail_write,
            };
            assert!(disable_with(&mut writer).is_err());
            let output = String::from_utf8_lossy(&writer.bytes);
            assert!(output.contains("\u{1b}[?1000l"));
            assert!(output.contains("\u{1b}[?2004l"));
        }
    }
}
