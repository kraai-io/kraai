# Agent Project Documentation

> **IMPORTANT**: This document should be kept up-to-date as the project evolves. If you make structural changes, add new tools, or modify the architecture, please update this file to maintain accuracy for future agents.

## Project Overview

This is a **work-in-progress AI agent framework** built in Rust with TypeScript bindings and an Electron desktop application. The goal is to create a modular, extensible system for building AI agents that can interact with various LLM providers and execute tools.

### Key Goals

- **Modular Provider System**: Support multiple LLM backends (OpenAI, Google, local models via mistral.rs)
- **Tool System**: Extensible tool framework for agent capabilities (file reading, etc.)
- **Multiple Interfaces**: Both desktop (Electron) and terminal (TUI) applications
- **TypeScript Bindings**: Rust core exposed to JS/TS via NAPI-RS for the desktop app
- **Custom Schema Format**: "Toon" format for tool schemas with compile-time generation

## Architecture

### Crate Structure

```
crates/
├── agent/              # Core agent management (Agent, AgentManager)
├── llm-providers/
│   ├── provider-core/  # Provider trait and manager
│   ├── provider-openai/# OpenAI API integration
│   └── provider-google/# Google AI integration (stub)
├── tools/
│   ├── tool-core/      # Tool trait and manager
│   └── tool-read-file/ # File reading tool implementation
├── toon-schema/        # Procedural macro for Toon schema generation
├── ts-bindings/        # NAPI-RS bindings for TypeScript
├── types/              # Shared types (ChatMessage, ChatRole, etc.)
└── tui/                # Terminal UI application (WIP, commented out in workspace)
```

### Application Structure

```
apps/
└── agent-desktop/      # Electron + React + Vite desktop application
    ├── src/
    │   ├── App.tsx     # Main React application
    │   ├── main.tsx    # Entry point
    │   └── styles/     # Tailwind CSS v4 styles
    └── package.json
```

## Technology Stack

### Core Technologies

- **Rust 1.88.0**: Core framework language
- **TypeScript/JavaScript**: Desktop application
- **Electron 39.4.0**: Desktop app framework
- **React 19.2.4**: UI library
- **Vite**: Build tool for desktop app
- **Tailwind CSS v4**: Styling

### Development Tools

| Tool | Purpose | Notes |
|------|---------|-------|
| **Nix** | Development environment | Uses flake.nix with flake-parts |
| **pnpm** | JS package manager | v10.28.0, workspace-enabled |
| **jj** | Version control | Jujutsu VCS (instead of git) |
| **just** | Task runner | See `justfile` for available commands |
| **Biome** | JS/TS linting & formatting | v1.9.4, configured in `biome.json` |
| **treefmt** | Multi-language formatting | Via nix, configured in `nix/treefmt.nix` |
| **cargo** | Rust build & package management | Via rust-overlay in nix |

### Key Dependencies

**Rust:**
- `tokio` - Async runtime
- `ratatui` - Terminal UI framework
- `async-openai` - OpenAI API client
- `serde` / `serde_json` - Serialization
- `color-eyre` - Error handling
- `llm-toolkit` - LLM utilities
- `toon-format` - Custom schema format
- `napi-rs` - Node.js bindings

**TypeScript:**
- `agent-ts-bindings` - Rust bindings (workspace package)
- `@radix-ui/react-slot` - UI primitives
- `lucide-react` - Icons
- `chokidar` - File watching
- `electron-vite` - Electron build tool

## Development Workflow

### Environment Setup

Enter the nix development shell:
```bash
nix develop
```

This provides:
- Rust toolchain (cargo, rust-analyzer)
- pnpm, nodejs
- just, pkg-config, openssl
- GTK/WebKit dependencies for Tauri (though currently using Electron)

### Common Commands

```bash
# Install dependencies
just install-deps
# or: pnpm install

# Full setup (install + build bindings)
just setup

# Build TypeScript bindings (required before running desktop app)
just build-bindings
just build-bindings-debug  # Debug build

# Desktop app
just dev-desktop      # Development mode
just build-desktop    # Production build
just typecheck-desktop # Type checking only

# Testing & Quality
just test-bindings    # Run binding tests
just test-all         # Run all tests
just lint             # Biome + Clippy
just format           # Format all code
just check            # Format + lint + test

# Full builds
just build-all        # Build bindings + desktop
just dev-all          # Build bindings then start desktop dev

# Cleanup
just clean            # Clean everything
just reset            # Clean + reinstall

# CI simulation
just localCI          # Run nix flake checks
```

### Project Configuration

**Config files to be aware of:**
- `flake.nix` - Nix flake definition
- `nix/devshell.nix` - Development shell packages
- `nix/treefmt.nix` - Formatting configuration
- `Cargo.toml` - Rust workspace definition
- `package.json` - Root JS configuration (Biome, pnpm)
- `pnpm-workspace.yaml` - pnpm workspace packages
- `biome.json` - Biome linter/formatter config
- `justfile` - Task definitions
- `crates/ts-bindings/config/config.toml` - Provider configuration (see `docs/config.md`)

## Current State (WIP)

### What's Working

- ✅ Basic workspace structure
- ✅ Provider abstraction (OpenAI working)
- ✅ Tool trait system
- ✅ Agent management (basic)
- ✅ TypeScript bindings (NAPI-RS)
- ✅ Desktop app skeleton (Electron + React)
- ✅ TUI skeleton (ratatui)
- ✅ Toon schema derive macro
- ✅ Configuration system (TOML-based)

### What's In Progress / TODO

- 🔨 Desktop app functionality (mostly stubbed)
  - Agent creation works
  - Provider/model listing needs implementation
  - Chat interface needs work
- 🔨 TUI application (commented out in workspace)
- 🔨 Tool system expansion (only read_file exists)
- 🔨 Google provider (stubbed)
- 🔨 Local model support via mistral.rs
- 🔨 Streaming responses need integration
- 🔨 Chat history management
- 🔨 Tool calling integration with LLMs

### Known Issues

- TUI crate is commented out in workspace `Cargo.toml` (line 11)
- Desktop app has many stubbed functions
- Biome doesn't fully support Tailwind v4 syntax (see `biome.json` excludes)
- Many TODO comments throughout codebase

## Important Patterns

### Adding a New Tool

1. Create new crate in `crates/tools/`
2. Implement `Tool` trait from `tool-core`
3. Use `toon-schema` derive macro for schema generation
4. Register in `ToolManager`

### Adding a New Provider

1. Create new crate in `crates/llm-providers/`
2. Implement `Provider` and `ProviderFactory` traits
3. Register factory in `ProviderManager`
4. Add configuration to `config.toml`

### Modifying TypeScript Bindings

Bindings are in `crates/ts-bindings/src/lib.rs`:
- Uses `napi` and `napi-derive` macros
- Run `just build-bindings` after changes
- Desktop app imports from `agent-ts-bindings` package

## Development Tips

1. **Always run `just build-bindings` after modifying Rust code** that affects bindings
2. **Use `just dev-all`** for the full development workflow
3. **The project uses jj, not git** - check `.jj/` directory for version control state
4. **Formatting is enforced** - run `just format` before committing
5. **Config is in TOML** - see `docs/config.md` for provider configuration format
6. **Nix is required** for consistent development environment

## Commit Workflow

This project uses **jj** (Jujutsu) for version control instead of git.

### Making Commits

1. **View your changes:**
   ```bash
   jj diff
   ```

2. **Describe the commit:**
   ```bash
   jj desc -m "commit message"
   ```

3. **For LLM-made commits:**
   - Use a single line description
   - Prefix with `[ai]` to indicate it was made by an AI
   - Example: `jj desc -m "[ai] fix typo in README"`

### Key jj Commands

| Command | Description |
|---------|-------------|
| `jj status` | Show current working copy state |
| `jj diff` | Show uncommitted changes |
| `jj desc -m "msg"` | Describe the current change |
| `jj log` | View commit history |
| `jj new` | Create a new empty change |
| `jj abandon` | Abandon the current change |

## File Locations Quick Reference

| Purpose | Location |
|---------|----------|
| Agent core | `crates/agent/src/lib.rs` |
| Provider trait | `crates/llm-providers/provider-core/src/lib.rs` |
| OpenAI provider | `crates/llm-providers/provider-openai/src/lib.rs` |
| Tool trait | `crates/tools/tool-core/src/lib.rs` |
| Read file tool | `crates/tools/tool-read-file/src/lib.rs` |
| Types | `crates/types/src/lib.rs` |
| TS bindings | `crates/ts-bindings/src/lib.rs` |
| Desktop app | `apps/agent-desktop/src/App.tsx` |
| TUI app | `crates/tui/src/app.rs` |
| Config docs | `docs/config.md` |

---

*Last updated: 2026-02-04*
*Maintainers: Please update this document when making architectural changes*
