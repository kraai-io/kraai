# AGENTS.md

## Task Completion Requirements

1. Run `just check` before committing to format, lint, and test

## Project Overview

Kraai is an AI agent framework.
Main goals:
- Find and implement new methods for using llms.
- Improve token efficiency.
- Improving the safety of using llms.

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
  kraai-agent/           - Core agent logic, profiles, prompts, and context state
  kraai-runtime/         - Agent runtime and script execution lifecycle
  kraai-persistence/     - Message, execution, effect, and turn persistence
  kraai-types/           - Shared protocol, profile, policy, and execution types
  kraai-script-protocol/ - Streaming XML envelope and result rendering
  kraai-sandbox/         - Capability-based process sandbox and output capture
  kraai-nushell-runtime/ - Embedded Nushell host and authenticated effect transport
  kraai-command-core/    - Native command declarations and registration contracts
  kraai-workspace-fs/    - Shared workspace filesystem operations
  commands/
    kraai-command-*/     - One crate per native model-facing Kraai command
  llm-providers/
    kraai-provider-core/ - LLM provider trait definitions
    kraai-provider-*/    - LLM provider implementations
  kraai-tui/             - Terminal UI
```

## Commands

read the justfile for commands to use

## Code Conventions

**Rust:**
- Dependencies use full version triple.

IMPORTANT: If you see a bad pattern in the code, don't be quick to copy it. It is best to squash bad patterns before they propogate. You should inform the user that you found the bad pattern, and then follow their instructions. Do not implement new features using these bad patterns without explicit confirmation.
IMPORTANT: We do not keep legacy code around. We don't care about backwards compatibility. This is a project in its demo / alpha stage. We ship fast, and fix fast.
This repo contains very heavy llm use, so some design decisions might not always be the best possible solutions.
God files should be broken up in the best way possible. They are an issue because it pollutes the context window and thus should be avoided and broken up.

## Dependencies

**Rust:**
- See `Cargo.toml` workspace.dependencies

## Other Notes

This project leans heavily into the Nix ecosystem. The CI builds and tests all nix outputs.
