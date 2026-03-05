# AGENTS.md

This file provides context for AI agents working on this codebase. It should be updated as the project evolves.

## Project Overview

A desktop agent application built with Rust backend and Electron/React frontend. Uses napi-rs for TypeScript bindings.

**Stack:**
- Build: pnpm (workspace), cargo, just (task runner)
- Linting: Biome (JS/TS/JSON/CSS), Clippy (Rust)

## Commands

read the justfile for commands to use

## Architecture

```
crates/
  agent/             - Core agent logic
  agent-runtime/     - Agent runtime
  persistence/       - Data persistence layer
  types/             - Shared type definitions
  agent-ts-bindings/ - napi-rs bindings for TypeScript
  llm-providers/
    provider-core/   - LLM provider trait definitions
    provider-google/ - Google LLM implementation
    provider-openai/ - OpenAI LLM implementation
  tools/
    tool-core/       - Tool trait definitions
    tool-read-file/  - File reading tool
  toon-schema/       - Schema definitions
  toon-schema-core/  - Core schema types
  tui/               - Terminal UI

apps/
  agent-desktop/     - Electron + React frontend
```

## Code Conventions

**Rust:**
- Strict clippy (`--all-targets -D warnings`)
- Workspace dependencies defined in root Cargo.toml

**TypeScript:**
- All TypeScript code was written by an LLM
- Use the types defined by the bindings in crates/agent-ts-bindings/index.d.ts

IMPORTANT: If you see a bad pattern in the code, don't be quick to copy it. It is best to squash bad patterns before they propogate. You should inform the user that you found the bad pattern, and then follow their instructions. Do not implement new features using these bad patterns without explicit confirmation.
IMPORTANT: We do not keep legacy code around. We don't care about backwards compatibility. This is a project in its demo / alpha stage. We ship fast, and fix fast.

## Learnings

## Dependencies

**Rust:** See `Cargo.toml` workspace.dependencies

**Node:** Read the package.json for each project including the root.

## Notes for Agents

1. Run `just lint` before committing to catch issues
2. TypeScript changes require `just typecheck-desktop`
3. Rust changes require `cargo clippy -- -D warnings`
5. After modifying agent-ts-bindings, rebuild with `just build-bindings-debug`
6. Update this file when:
   - Architecture changes occur
   - New commands become frequently used
