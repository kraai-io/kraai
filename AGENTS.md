# AGENTS.md

This file provides context for AI agents working on this codebase. It should be updated as the project evolves.

## Project Overview

A desktop agent application built with Rust backend and Electron/React frontend. Uses napi-rs for TypeScript bindings.

**Stack:**
- Backend: Rust (edition 2024, MSRV 1.88.0)
- Frontend: Electron 39 + React 19 + TypeScript + Tailwind CSS v4
- Build: pnpm (workspace), cargo, just (task runner)
- Linting: Biome (JS/TS/JSON/CSS), Clippy (Rust)
- UI: Radix UI primitives, Lucide icons, cmdk

## Commands

| Task | Command |
|------|---------|
| Setup | `just setup` |
| Build all | `just build-all` |
| Dev mode | `just dev` |
| Lint | `just lint` |
| Format | `just format` |
| Test all | `just test-all` |
| Typecheck | `just typecheck-desktop` |
| Clean | `just clean` |

## Architecture

```
crates/
  agent/             - Core agent logic
  agent-runtime/     - Agent runtime
  persistence/       - Data persistence layer
  types/             - Shared type definitions
  ts-bindings/       - napi-rs bindings for TypeScript
  llm-providers/
    provider-core/   - LLM provider trait definitions
    provider-google/ - Google LLM implementation
    provider-openai/ - OpenAI LLM implementation
  tools/
    tool-core/       - Tool trait definitions
    tool-read-file/  - File reading tool
  toon-schema/       - Schema definitions
  toon-schema-core/  - Core schema types
  tui/               - Terminal UI (disabled in workspace)

apps/
  agent-desktop/     - Electron + React frontend
```

## Code Conventions

**Rust:**
- Edition 2024, strict clippy (`-D warnings`)
- Workspace dependencies defined in root Cargo.toml

**TypeScript:**
- All TypeScript code was written by an LLM
- Single quotes, no semicolons, trailing commas (ES5)
- Use `import type` for type-only imports
- Line width: 100

**Imports:**
- React components use `"use client"` directive where needed
- Radix UI for component primitives

## Learnings

<!-- Update this section as you discover patterns, gotchas, and solutions -->

### Discovered Patterns
- LLM providers follow a trait-based pattern via provider-core

### Gotchas
- (Add non-obvious issues encountered)

### Solutions Applied
- (Document solutions to problems faced)

## Dependencies

**Rust:** See `Cargo.toml` workspace.dependencies

Key dependencies: async-openai, tokio, serde, specta, llm-toolkit, ratatui

**Node:**
- Root: biome only
- ts-bindings: napi-rs, ava (testing)
- agent-desktop: electron, react, react-dom, tailwind v4, radix-ui, lucide-react, cmdk, react-markdown, katex

## Build Targets

Supported napi targets in `crates/ts-bindings/package.json`:
- Windows: x86_64, aarch64, i686 (msvc)
- macOS: x86_64, aarch64 (darwin)
- Linux: x86_64, aarch64 (gnu/musl)
- Other: freebsd, android, wasm32-wasip1-threads

## Notes for Agents

1. Run `just lint` before committing to catch issues
2. TypeScript changes require `just typecheck-desktop`
3. Rust changes require `cargo clippy -- -D warnings`
4. After modifying ts-bindings, rebuild with `just build-bindings`
5. Update this file when:
   - New patterns are discovered
   - Non-obvious issues are solved
   - Architecture changes occur
   - New commands become frequently used
