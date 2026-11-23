use clap::Parser;
use color_eyre::eyre::Result;
use kraai_runtime::RuntimeBuilder;
use std::path::PathBuf;

use crate::app::{App, StartupOptions};

mod app;
mod clipboard_image;
mod components;
mod duration;
mod terminal_cursor;
mod terminal_features;
mod terminal_loss;
#[cfg(test)]
#[cfg(unix)]
mod terminal_session_tests;
mod theme;

struct TerminalSessionGuard {
    active: bool,
    cursor: std::sync::Arc<terminal_cursor::CursorOverride>,
}

impl TerminalSessionGuard {
    fn new() -> Self {
        Self {
            active: true,
            cursor: Default::default(),
        }
    }

    fn install_panic_hook(&self) {
        let cursor = self.cursor.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = ratatui::try_restore();
            let _ = cursor.restore();
            previous(info);
        }));
    }

    fn restore(&mut self) -> std::io::Result<()> {
        if !self.active {
            return self.cursor.restore();
        }
        let feature_result = terminal_features::disable();
        ratatui::restore();
        let cursor_result = self.cursor.restore();
        self.active = false;
        feature_result.and(cursor_result)
    }
}

impl Drop for TerminalSessionGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Debug, Parser)]
#[command(name = "kraai", version = kraai_runtime::VERSION)]
struct Cli {
    #[arg(long = "agent-profile", value_name = "ID")]
    agent_profile: Option<String>,

    #[arg(long, value_name = "TEXT")]
    message: Option<String>,

    #[arg(long = "provider-config", value_name = "PATH")]
    provider_config: Option<PathBuf>,
}

fn main() -> Result<()> {
    if let Some(exit_code) = clipboard_image::run_internal() {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = kraai_runtime::run_internal_process() {
        std::process::exit(exit_code);
    }
    color_eyre::install()?;

    let cli = Cli::parse();

    let runtime_builder = RuntimeBuilder::new(kraai_providers::DefaultProviders)
        .use_current_executable_as_nushell_host();
    let runtime_builder = if let Some(path) = cli.provider_config.clone() {
        runtime_builder.provider_config_path(path)
    } else {
        runtime_builder
    };
    let runtime = runtime_builder.build();

    let startup_options = StartupOptions {
        agent_profile_id: cli.agent_profile,
        message: cli.message,
    };

    let mut app = App::new(runtime, startup_options);
    app.set_sandbox_check(kraai_sandbox::availability_warning);

    crossterm::terminal::enable_raw_mode()?;
    app.request_cursor_preferences();
    let mut terminal = ratatui::init();
    let mut terminal_guard = TerminalSessionGuard::new();
    terminal_guard.install_panic_hook();
    terminal_features::enable()?;

    let result = app.run(&mut terminal, &terminal_guard.cursor);

    if terminal_loss::detach_if_lost() {
        terminal_guard.active = false;
        return Ok(());
    }
    drop(terminal);
    let restore_result = terminal_guard.restore();

    if result.is_ok()
        && let Some(summary) = app.exit_token_usage_summary()
    {
        println!("{summary}");
    }

    result.and(restore_result.map_err(Into::into))
}

#[cfg(test)]
mod tests;
