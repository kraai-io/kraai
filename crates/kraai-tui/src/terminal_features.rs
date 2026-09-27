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
    #[cfg(unix)]
    execute!(
        stdout(),
        ratatui::crossterm::event::PopKeyboardEnhancementFlags
    )?;
    execute!(stdout(), DisableMouseCapture, DisableBracketedPaste)
}
