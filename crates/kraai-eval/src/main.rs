#![forbid(unsafe_code)]

mod cli;

fn main() -> color_eyre::eyre::Result<std::process::ExitCode> {
    color_eyre::install()?;
    cli::run()
}
