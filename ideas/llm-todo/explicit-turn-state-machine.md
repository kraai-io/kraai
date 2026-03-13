# Explicit Turn State Machine

## Why this matters

Turn lifecycle is currently spread across multiple booleans, maps, and ad hoc cleanup calls. That makes restart behavior, cancellation, continuation, and tool execution harder to reason about and easier to regress.

## Current gap

The effective turn state is inferred from combinations of:

1. `streaming_messages` in `crates/agent/src/lib.rs`
2. `pending_tool_calls` in `SessionRuntimeState`
3. `active_turn_profile`
4. runtime `active_streams` in `crates/agent-runtime/src/lib.rs`
5. several manual cleanup paths such as `clear_active_turn()`

Those pieces are coherent today, but the invariants are implicit.

## Goal

Represent each session turn as a single explicit state machine with well-defined transitions and derived UI/runtime flags.

## Plan

1. Define a `TurnState` enum with a narrow set of states:
   - `Idle`
   - `Streaming`
   - `AwaitingToolApproval`
   - `ExecutingTools`
   - `Continuing`
   - `Failed`
2. Move transition logic into reducer-like methods on `AgentManager` instead of scattering updates across call sites.
3. Derive `profile_locked`, `waiting_for_approval`, and `is_streaming` from `TurnState` instead of maintaining them indirectly.
4. Make stream start, completion, failure, cancellation, and tool execution all require valid prior states.
5. Emit structured transition errors when a caller tries to do something illegal, such as executing tools while not awaiting approval.
6. Keep the first migration internal to Rust and avoid changing bindings until the state machine is stable.

## Milestones

1. Introduce `TurnState` alongside current fields.
2. Route stream lifecycle through state transitions.
3. Route tool approval/execution through the same transitions.
4. Delete obsolete flags and cleanup helpers once tests pass.

## Validation

1. Add table-driven transition tests for legal and illegal transitions.
2. Re-run existing stream cancellation and continuation tests against the new state machine.
3. Add assertions that session-derived flags always match the current `TurnState`.

## Risks

This refactor will touch a lot of control flow. The safe path is to preserve public behavior and change state representation first, not event shape or API shape.
