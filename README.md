# Kraai

Kraai is an experimental AI agent framework focused on three things:

- finding better ways to use LLMs
- reducing token waste
- ensuring agents are safe to run fully autonomously

The repository is still early-stage and opinionated. The current implementation centers on a terminal-first agent runtime with explicit tool permissions, persistent sessions, configurable provider backends, and workspace-scoped agent profiles.

## Current Status

Kraai is a work in progress. The project is moving quickly and does not optimize for backward compatibility yet.

The TUI should have enough basic features for use, but it is not polished.

The runtime is built to be frontend agnostic. A napi-rs TypeScript binding is planned for once the runtime api is stabilized.

## What Exists Today

- A terminal UI binary: `kraai`
- A background runtime that manages providers, tools, sessions, and streaming
- Persistent local state under `~/.kraai`
- Built-in agent profiles for read-only planning and code-editing workflows
- Explicit tool approval flows with risk levels
- OpenAI Chat Completions support
- OpenAI Codex support backed by ChatGPT/Codex subscription auth
- Workspace and global agent profile overrides
- A compile-time TOON schema crate in [`crates/kraai-toon-schema`](crates/kraai-toon-schema/README.md)

## Quickstart

Kraai is already packaged with Nix. The flake exposes the TUI as the default package, so you can build or run it directly with Nix instead of setting up Cargo manually.

```bash
nix run github:kraai-io/kraai
```

This repo is most comfortable inside the Nix dev shell.

### 1. Enter the dev environment

```bash
nix develop
```

If you are not using Nix, use Rust `1.88.0` and expect to install the native libraries referenced in `nix/devshell.nix`.

### 2. Configure a provider

By default Kraai reads provider settings from `~/.kraai/providers.toml`. You can also point to another file with `--provider-config`.

Example `~/.kraai/providers.toml`:

```toml
[[providers]]
id = "openai"
type_id = "openai"

[providers.config]
env_var_api_key = "OPENAI_API_KEY"
only_listed_models = true

[[models]]
id = "gpt-5.4"
provider_id = "openai"

[models.config]
name = "GPT-5.4"
```

Then export your API key:

```bash
export OPENAI_API_KEY=...
```

`openai-codex` is also available. That provider uses ChatGPT/Codex account auth instead of an API key and is managed from the TUI provider screen.

### 3. Run Kraai

```bash
cargo run -p kraai-tui --bin kraai
```

Useful flags:

- `--provider-config <PATH>`: use a non-default `providers.toml`
- `--provider <ID>`: choose a provider in CI mode
- `--model <ID>`: choose a model in CI mode
- `--agent-profile <ID>`: choose an agent profile in CI mode
- `--message <TEXT>`: submit a prompt immediately in CI mode
- `--auto-approve`: auto-approve tool calls up to the profile risk limit
- `--ci`: run without the interactive terminal UI

Example CI invocation:

```bash
cargo run -p kraai-tui --bin kraai -- \
  --ci \
  --provider openai \
  --model gpt-5.4 \
  --agent-profile build-code \
  --message "Inspect the workspace and summarize the architecture"
```

## Agent Profiles

Kraai currently ships with two built-in profiles:

- `plan-code`: read-only planning and investigation
- `build-code`: implementation with workspace write access

Profiles define:

- the system prompt
- which tools are available
- the default tool approval threshold

You can override or add profiles in two places:

- global: `~/.kraai/agents.toml`
- workspace-local: `.kraai/agents.toml`

Workspace profiles override global and built-in profiles with the same `id`.

## Tooling Model

The runtime registers a small, explicit tool set:

- `close_file`
- `read_file`
- `list_files`
- `open_file`
- `search_files`
- `edit_file`

Tool calls are assessed against a risk model before execution. Higher-risk operations require explicit approval unless the active profile and run mode allow auto-approval.

## Persistence

Kraai stores local state in `~/.kraai`:

- `providers.toml`: provider and model configuration
- `agents.toml`: optional global agent profiles
- `data/`: sessions and message history
- `logs/`: runtime logs

This makes sessions resumable and keeps message state separate from the current workspace.

## Development

Common commands are defined in `justfile`:

```bash
just check
```

`just check` runs formatting, clippy, and tests.

Nix-specific checks:

```bash
just localCI
```

## License

Apache-2.0
