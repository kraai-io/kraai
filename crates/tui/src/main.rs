#![forbid(unsafe_code)]

use agent_runtime::{Event, RuntimeBuilder};
use clap::Parser;
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

use crate::app::{App, StartupOptions};

mod app;
mod components;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, value_name = "ID")]
    provider: Option<String>,

    #[arg(long, value_name = "ID")]
    model: Option<String>,

    #[arg(long = "agent-profile", value_name = "ID")]
    agent_profile: Option<String>,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    let (event_tx, event_rx): (Sender<Event>, _) = bounded(100);

    let callback = move |event: Event| {
        let _ = event_tx.send(event);
    };

    let runtime = RuntimeBuilder::new(callback).build();

    let startup_options = StartupOptions {
        provider_id: cli.provider,
        model_id: cli.model,
        agent_profile_id: cli.agent_profile,
    };

    let mut app = App::new(runtime, event_rx, startup_options);

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
