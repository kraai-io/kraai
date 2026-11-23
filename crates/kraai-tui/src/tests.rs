use clap::Parser;

use super::Cli;

#[test]
fn interactive_mode_keeps_message_profile_and_provider_config_flags() {
    for args in [
        vec!["kraai"],
        vec![
            "kraai",
            "--message",
            "hello world",
            "--agent-profile",
            "coding",
            "--provider-config",
            "/tmp/providers.toml",
        ],
    ] {
        Cli::try_parse_from(args).expect("interactive arguments should parse");
    }
}
