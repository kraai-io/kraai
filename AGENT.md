This file is for the use of the Agent. The Agent can freely modify this file however it wants. The Agent should be editing it. The Agent should use this to save state across sessions and context windows.

# AGENT.md - Project State Tracking

## Project Overview
**AI Agent Framework and Desktop Application**
- Rust-based backend with TypeScript/Electron frontend
- Multi-provider LLM support (OpenAI-compatible, Google/Gemini)
- Extensible tool system with custom "Toon" schema format
- NAPI bindings for Rust-Node.js bridge

## Workspace Structure

### Rust Crates (`crates/`)
- `agent/` - Core agent management, conversation history, message routing
- `types/` - Shared type definitions
- `llm-providers/provider-core/` - LLM provider abstraction (factory pattern)
- `llm-providers/provider-openai/` - OpenAI-compatible provider
- `llm-providers/provider-google/` - Google/Gemini provider
- `tools/tool-core/` - Trait-based tool system framework
- `tools/tool-read-file/` - File reading tool implementation
- `toon-schema/` - Proc-macro derive for Toon format schemas
- `ts-bindings/` - NAPI-based Node.js bindings
- `tui/` - Terminal UI (currently commented out in workspace)

### Applications (`apps/`)
- `agent-desktop/` - Electron-based desktop application (React UI)

## Build Commands

```bash
# Build TypeScript bindings (must be done before desktop)
just build-bindings

# Run desktop app in development mode
just dev-desktop

# Build desktop app for production
just build-desktop

# Package for distribution
just package-desktop-linux    # Linux AppImage
just package-desktop-macos    # macOS DMG
just package-desktop-windows  # Windows NSIS
```

## Configuration

### Provider Configuration
- **Location:** `~/.agent-desktop/providers.toml`
- **Format:** TOML with provider and model registration
- **Hot-reload:** File watcher automatically reloads on changes

## Development Workflow

### Adding a New Tool
1. Create new crate in `crates/tools/tool-<name>/`
2. Implement `Tool` trait from `tool-core`
3. Add to `ts-bindings/src/lib.rs` tool registry
4. Rebuild bindings: `just build-bindings`

### Adding a New Provider
1. Create new crate in `crates/llm-providers/provider-<name>/`
2. Implement `Provider` and `ProviderFactory` traits
3. Register in provider factory
4. Update `providers.toml` configuration

## Known Issues & Limitations

- **Config Watcher Bug:** Issue noted in `ts-bindings/src/lib.rs:225` - may need attention
- **TUI Crate:** Commented out in workspace, not currently active
- **Toon Schema Limitations:** Does not support enums, nested structs, maps, or tuples

## Cross-Session Notes

### Current Session
- **Date:** 2026-02-11
- **Status:** Initial project exploration and AGENT.md setup

### Build Dependencies
1. Rust crates must be built first
2. TypeScript bindings generated from Rust (NAPI)
3. Desktop app depends on bindings

### Testing
- `toon-schema` has compile-fail tests for derive macro
- Run bindings tests after changes to core crates

## Release Artifacts
- **Output Directory:** `releases/`
- **Supported Platforms:** Linux, macOS, Windows
- **Formats:** AppImage (Linux), DMG (macOS), NSIS (Windows)
