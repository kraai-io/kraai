#![forbid(unsafe_code)]

use agent_runtime::RuntimeBuilder;
use clap::{CommandFactory, Parser, error::ErrorKind};
use color_eyre::eyre::Result;
use ratatui::crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
};
use std::io::stdout;
use std::path::PathBuf;

use crate::app::{App, StartupOptions};

mod app;
mod components;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    ci: bool,

    #[arg(long)]
    auto_approve: bool,

    #[arg(long, value_name = "ID")]
    provider: Option<String>,

    #[arg(long, value_name = "ID")]
    model: Option<String>,

    #[arg(long = "agent-profile", value_name = "ID")]
    agent_profile: Option<String>,

    #[arg(long, value_name = "TEXT")]
    message: Option<String>,

    #[arg(long = "provider-config", value_name = "PATH")]
    provider_config: Option<PathBuf>,
}

impl Cli {
    fn validate(self) -> Result<Self, clap::Error> {
        if !self.ci {
            return Ok(self);
        }

        let missing = [
            (self.provider.is_none(), "--provider <ID>"),
            (self.model.is_none(), "--model <ID>"),
            (self.agent_profile.is_none(), "--agent-profile <ID>"),
            (self.message.is_none(), "--message <TEXT>"),
        ]
        .into_iter()
        .filter_map(|(is_missing, flag)| is_missing.then_some(flag))
        .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(self)
        } else {
            Err(Self::command().error(
                ErrorKind::MissingRequiredArgument,
                format!("--ci requires {}", missing.join(", ")),
            ))
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse().validate().unwrap_or_else(|error| error.exit());

    let runtime_builder = RuntimeBuilder::new();
    let runtime_builder = if let Some(path) = cli.provider_config.clone() {
        runtime_builder.provider_config_path(path)
    } else {
        runtime_builder
    };
    let runtime = runtime_builder.build();

    let startup_options = StartupOptions {
        ci: cli.ci,
        auto_approve: cli.auto_approve,
        provider_id: cli.provider,
        model_id: cli.model,
        agent_profile_id: cli.agent_profile,
        message: cli.message,
    };

    let mut app = App::new(runtime, startup_options);

    if cli.ci {
        return app.run_ci();
    }

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

#[cfg(test)]
mod tests {
    use clap::{Parser, error::ErrorKind};

    use super::Cli;

    #[test]
    fn ci_requires_provider_model_agent_profile_and_message() {
        let error = Cli::try_parse_from(["tui", "--ci"])
            .and_then(Cli::validate)
            .expect_err("--ci without required args should fail");

        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
        let rendered = error.to_string();
        assert!(rendered.contains("--provider <ID>"));
        assert!(rendered.contains("--model <ID>"));
        assert!(rendered.contains("--agent-profile <ID>"));
        assert!(rendered.contains("--message <TEXT>"));
    }

    #[test]
    fn ci_accepts_complete_argument_set() {
        let cli = Cli::try_parse_from([
            "tui",
            "--ci",
            "--auto-approve",
            "--provider",
            "openai-chat-completions",
            "--model",
            "gpt-4o-mini",
            "--agent-profile",
            "build-code",
            "--message",
            "hello world",
        ])
        .and_then(Cli::validate)
        .expect("complete ci args should parse");

        assert!(cli.ci);
        assert!(cli.auto_approve);
        assert_eq!(cli.provider.as_deref(), Some("openai-chat-completions"));
        assert_eq!(cli.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(cli.agent_profile.as_deref(), Some("build-code"));
        assert_eq!(cli.message.as_deref(), Some("hello world"));
    }

    #[test]
    fn parses_provider_config_path_argument() {
        let cli = Cli::try_parse_from(["tui", "--provider-config", "/tmp/custom-providers.toml"])
            .and_then(Cli::validate)
            .expect("provider config arg should parse");

        assert_eq!(
            cli.provider_config.as_deref(),
            Some(std::path::Path::new("/tmp/custom-providers.toml"))
        );
    }
}
