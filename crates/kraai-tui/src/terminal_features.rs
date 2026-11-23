use std::io::{Result, stdout};

use ratatui::crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
};

pub(crate) fn enable() -> Result<()> {
    enable_with(&mut stdout())
}

fn enable_with(writer: &mut impl std::io::Write) -> Result<()> {
    execute!(writer, EnableMouseCapture, EnableBracketedPaste)?;
    #[cfg(unix)]
    execute!(
        writer,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;
    Ok(())
}

pub(crate) fn disable() -> Result<()> {
    disable_with(&mut stdout())
}

fn disable_with(writer: &mut impl std::io::Write) -> Result<()> {
    #[cfg(unix)]
    let keyboard = execute!(writer, crossterm::event::PopKeyboardEnhancementFlags);
    #[cfg(not(unix))]
    let keyboard = Ok(());
    let mouse = execute!(writer, DisableMouseCapture);
    let paste = execute!(writer, DisableBracketedPaste);
    keyboard.and(mouse).and(paste)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
