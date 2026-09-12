# kraai

Kraai is an experimental AI agent framework with a terminal interface. It uses
Nushell as the language for agent actions, so a model can run commands, filter
structured output, and edit files in a single script.

We're exploring how agents can work with less repeated context and more control
over what they can access. This is an early work in progress. Expect breaking
changes.

## Run it

From the directory you want to work in, run:

```sh
nix run github:kraai-io/kraai
```

The Nix flake currently targets `x86_64-linux`. If you already have Kraai
installed, run `kraai`.

In the terminal UI, use `/providers` to configure a provider, `/model` to choose
a model, and `/agent` to choose an agent profile. Enter your task to start a
conversation. `/help` lists the available commands, and `/sessions` opens
previous sessions.

## Agent execution

The model writes Nushell scripts that combine external programs, structured
pipelines, and Kraai's native commands. Each script has a timeout and requests
permissions before it runs. Profiles define filesystem and network access, along
with whether the runtime denies escalation, asks for approval, or allows it.

Files can stay open in the model's context. Before each request, Kraai reads their
latest contents from disk, so edits don't leave a trail of full-file copies in
the conversation. The model opens and closes these files with `kraai-open-files`
and `kraai-close-files`, and makes exact text edits with `kraai-edit-file`.
Keeping one copy saves context space. Whether it also lowers token costs depends
on edits and cache hits.

The runtime also refreshes workspace instructions before each model request. It
persists script source, output, context changes, and execution status for crash
recovery. A failed script does not roll back commands that already completed.

The details live in [Script protocol](docs/script-protocol.md) and
[File context and token cost](docs/file-context.md).

## Development

From a checkout, enter the development environment and run the terminal app:

```sh
nix develop
cargo run --bin kraai
```

Before committing, run:

```sh
just check
```

This regenerates `Cargo.nix`, checks formatting, runs Clippy, and runs the tests.
The workspace separates agent logic, execution, persistence, sandboxing, model
providers, and the terminal UI into crates under `crates/`.

`kraai-eval` runs agents against fixed Git fixtures and grades their submissions
in fresh workspaces. Grading tests are kept out of the agent sandbox. Results and
process logs are stored by experiment identity for reuse.

## License

Apache-2.0
