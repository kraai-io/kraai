# Kraai

`kraai` is an agentic runtime for llm tool calling.
The main goals of the project are improving token efficiency and model accuracy.

## Features

- **Toon Formatted Tool Calls**: All tool calls use less context
- **Dynamic System Prompt**: Token caching works with an ever changing system prompt
- **Stateful Tools**: Tools can cause system prompt injection with updating context every turn
- **Small Tool Set**: open_file, edit_file, bash, search_files, list_files, close_file

## Usage

Run in the terminal
```bash
kraai
```

Run through nix
```bash
nix run github:kraai-io/kraai
```

Build with cargo
```bash
cargo run
```

## Configuration

Most configuration can be done through the UI. Currently all configuration is unstable and might change at any time.

## License

Apache-2.0
