# Agent Instructions

## Context
This repository contains code managed by AI agents. This file provides guidance for agents working on this codebase.

## Self-Learning Clause

As an AI agent working on this codebase, you are expected to:

1. **Learn from interactions**: Observe patterns in the codebase and adapt your approach to match existing conventions, styles, and architectures.

2. **Improve over time**: Apply lessons learned from previous tasks to future work. If you make a mistake, commit it to memory and avoid repeating it.

3. **Document learnings**: If you discover important patterns, architectural decisions, or gotchas specific to this codebase, document them in relevant files or comments.

4. **Ask clarifying questions**: When requirements are ambiguous, prefer asking for clarification over making assumptions. Use this to build a better mental model of the project.

5. **Follow conventions**: Before writing new code, examine existing code in the same area and follow established patterns for naming, structure, and style.

6. **Verify assumptions**: Test your understanding of the codebase by running tests, checking documentation, and validating outputs.

7. **Document aha moments**: After/during work, edit this AGENT.md file to capture any durable, reusable learnings or recommendations that would make future work faster or cleaner.

8. **Proactive documentation**: Do NOT wait to be asked to update AGENT.md. Automatically document learnings as you work:
   - When you discover an important pattern, gotcha, or workflow
   - When you solve a non-obvious problem
   - When you establish a new convention or approach
   - Add to the "Learnings from Recent Work" section without prompting

9. **Maintain this file**: You are allowed (and encouraged) to remove or modify anything in this AGENT.md if you find it is incorrect or has changed.

## General Guidelines

- **Be thorough**: Run lint, type checks, and tests when available before completing tasks.
- **Be safe**: Never commit secrets, credentials, or sensitive information.
- **Be collaborative**: Only commit changes when explicitly requested by the user.
- **Be proactive**: If you identify potential issues or improvements, suggest them to the user.

## Learnings from Recent Work

### toon-schema crate

**What it does**: Proc-macro derive that generates Toon format schema documentation from Rust structs. Used for LLM tool documentation with compile-time validation.

**Key architectural patterns**:
- Three-phase architecture: `parse.rs` → IR (`ir.rs`) → `lib.rs` (codegen)
- Uses `syn` for parsing, `quote` for code generation
- Compile-time JSON validation via `serde_json::from_str`
- Static string generation with `Box::leak` (memory tradeoff for convenience)

**Gotchas discovered**:
- Proc-macros can't easily detect external enum definitions, so enum support uses explicit `#[toon_schema(variants = "A|B|C")]` attribute
- Type inference can fail in `parse.rs` when collecting to empty Vec - need explicit type annotation
- Unused imports in proc-macros are hard to catch without `cargo check`

**Testing approach**:
- Unit tests in `tests/` for successful cases
- Compile-fail tests with `.stderr` files for error validation
- Use `type` aliases when testing enum support (type name becomes the enum identifier)

**JJ workflow**:
- `jj st` and `jj diff` to review changes
- `jj describe -m "[ai] message"` to set commit message (this "closes" the current commit)
- `jj new` to create new working commit (MUST run AFTER `jj describe` to start a new commit!)
- **Key**: Always run `jj new` AFTER describing, not before. The workflow is: make changes → describe → new → repeat

## Tooling

When working on this project, prefer using available tools and commands defined in the project (e.g., `npm run lint`, `npm run typecheck`, etc.) to ensure code quality.

**For toon-schema**:
- `cargo test -p toon-schema --test <test_name>` - Run specific tests
- `cargo run -p toon-schema --example <example>` - Run examples
