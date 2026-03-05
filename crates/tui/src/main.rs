use agent_runtime::{Event, RuntimeBuilder};
use color_eyre::eyre::Result;
use crossbeam_channel::{Sender, bounded};
use ratatui::crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use std::io::stdout;

use crate::app::App;

mod app;
mod components;

fn main() -> Result<()> {
    color_eyre::install()?;

    let (event_tx, event_rx): (Sender<Event>, _) = bounded(100);

    let callback = move |event: Event| {
        let _ = event_tx.send(event);
    };

    let runtime = RuntimeBuilder::new(callback).build();

    let mut app = App::new(runtime, event_rx);

    let terminal = ratatui::init();
    execute!(
        stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;

    let result = app.run(terminal);

    execute!(
        stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags
    )?;
    ratatui::restore();

    result
}
