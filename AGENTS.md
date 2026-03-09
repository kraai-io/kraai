# AGENTS.md

## Task Completion Requirements

1. Run `just lint` before committing to catch issues
2. TypeScript changes require `just typecheck-desktop`
3. Rust changes require `cargo clippy --all-targets -- -D warnings` and `cargo nextest run`
4. After modifying agent-ts-bindings, rebuild with `just build-bindings-debug`

## Project Overview

Agent (not named yet / unbranded) is a ai agent framework.
Main goals:
- Find and implement new methods for using llms.
- Improve token efficiency.
- Improving the safety of using llms.

There are currently two frontends in this repo for using agent: a tui and a electron app.

This repository is a VERY EARLY WIP. Proposing sweeping changes that improve long-term maintainability is encouraged.

## Core Priorities

1. Performance first.
2. Reliability first.
3. Keep behavior predictable under load and during failures (session restarts, reconnects, partial streams).

If a tradeoff is required, choose correctness and robustness over short-term convenience.

## Maintainability

Long term maintainability is a core priority. If you add new functionality, first check if there are shared logic that can be extracted to a separate module. Duplicate logic across mulitple files is a code smell and should be avoided. Don't be afraid to change existing code. Don't take shortcuts by just adding local logic to solve a problem.

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
    provider-*/      - LLM provider implementations
  tools/
    tool-core/       - Tool trait definitions
    tool-*/          - Tool definitions
  toon-schema/       - Schema definitions
  toon-schema-core/  - Core schema types
  tui/               - Terminal UI

apps/
  agent-desktop/     - Electron + React frontend
```

## Commands

read the justfile for commands to use

## Code Conventions

**Rust:**
- Dependencies use full version triple.

IMPORTANT: If you see a bad pattern in the code, don't be quick to copy it. It is best to squash bad patterns before they propogate. You should inform the user that you found the bad pattern, and then follow their instructions. Do not implement new features using these bad patterns without explicit confirmation.
IMPORTANT: We do not keep legacy code around. We don't care about backwards compatibility. This is a project in its demo / alpha stage. We ship fast, and fix fast.
This repo contains very heavy llm use, so some design decisions might not always be the best possible solutions.

## Dependencies

**Rust:**
- See `Cargo.toml` workspace.dependencies
**TypeScript:**
- Use the types defined by the bindings in crates/agent-ts-bindings/index.d.ts
- Read the package.json for each project including the root.

## Other Notes

This project leans heavily into the Nix ecosystem. The CI builds and tests all nix outputs.
Any suggestions about new features or improvements should be placed in ideas/llm-unchecked/name-of-idea.md
