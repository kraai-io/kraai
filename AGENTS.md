# AGENTS.md

Kraai is an AI agent harness. Goals: find new ways to use LLMs, cut token cost, and make LLM use safer.

Most of the repo was written by agents, so existing code is not proof of a deliberate decision. Don't copy a bad pattern. Point it out and ask before building on it.

## Priorities

1. Correctness and reliability, including session restarts, reconnects, and partial streams.
2. Performance. Keep TUI startup minimal. Keep the TUI responsive to input.
3. Model cost. The cost drivers are turn count, output and reasoning tokens, context window usage, and uncached input tokens.

## Scope

- Don't add flags, keybinds, subcommands, config files, or just recipes the task didn't call for. A command earns its place only if someone would actually run it.
- Fix clear bugs and perf problems without asking. If a behavior might be intentional, ask before changing it.
- Don't restrict a user's own choices to protect them from themselves. For example, requiring HTTPS for providers blocks valid HTTP setups.
- Linux, macOS, and Windows should have the same features and behavior. When a platform can't match, tell the user and work out a solution with them.
- No legacy code. When you replace something, delete the old path in the same change instead of keeping it behind a fallback or compat shim. Migrations and version bumps are fine when the data or protocol actually changes. Write experiments as production code.

## Code

- Break up god files. Shared logic goes in a shared module, not copied into each caller.
- Fix warnings instead of silencing them.
- Delete tests that don't test anything real.
- Unit tests for `foo.rs` go in `foo/tests.rs`, declared as `#[cfg(test)] mod tests;`. Never inline a test module. Split large ones into `foo/tests/*.rs`.
- Comments only for code so unusual that a reader can't infer why it must be written that way. Never comment to explain a change.
- Rust dependencies go in `[workspace.dependencies]` with a full version triple.

## Model-facing text

This covers system prompts, tool descriptions, tool output, and error messages the model reads.

- Keep it minimal. Every instruction biases the model, so only say what it can't work out on its own.
- Make it intuitive. Reasoning the model spends figuring out its instructions is reasoning it doesn't spend on the task, and another chance to misread them.

## Checks and CI

- `just check` and `nix flake check` are expensive, and CI covers both, so you usually don't need to run them. Run `just generate-cargo-nix` after changing dependencies and `just format` before finishing. Run narrow checks for what you touched: `just lint`, `cargo nextest run -p <crate> --all-features`, or the matching recipe in the `justfile`.
- Fix flaky tests, don't rerun them.
- CI tests Linux, macOS, and Windows.
- Profiles live in `Cargo.toml`. Rustflags and nightly-only settings live in `.cargo/config.toml`.

## Evals

Only run evals when asked. Output goes to the default eval directory, never `/tmp` or a scratchpad.
