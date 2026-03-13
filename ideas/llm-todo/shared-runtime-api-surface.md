# Shared Runtime API Surface

## Why this matters

Public runtime DTOs are currently mirrored manually between Rust runtime code and N-API bindings. That is tolerable at small scale, but it will drift as soon as more runtime events or more frontends appear.

## Current gap

`agent-runtime` defines `Session`, `WorkspaceState`, `Event`, and profile-related DTOs, while `agent-ts-bindings` redefines corresponding N-API-facing types and conversion code. The same semantic model exists in two places with no shared source of truth.

## Goal

Create one serializable runtime API surface that all frontends and bindings adapt from, rather than redefining the same DTOs repeatedly.

## Plan

1. Create a shared runtime API module or crate containing:
   - session summaries
   - workspace state
   - profile summaries and warnings
   - runtime events
   - provider settings DTOs where useful
2. Derive `Serialize` and `Deserialize` for the shared API types so they can be reused across bindings and tests.
3. Update `agent-runtime` to emit the shared API types directly.
4. Reduce `agent-ts-bindings` to thin N-API exposure wrappers instead of semantic remapping.
5. Add event round-trip tests so adding a new event requires updating one shared definition instead of multiple mirrored ones.

## Milestones

1. Move profile and session DTOs first.
2. Move `Event` second.
3. Move settings DTOs last if the separation still feels right.

## Validation

1. Compile-time failures should catch any newly added runtime event that has no binding exposure.
2. Snapshot the serialized form of key events and session DTOs.
3. Verify the bindings surface stays source-compatible where needed.

## Risks

The main risk is over-extracting too much too early. Keep the shared surface limited to stable client-facing runtime data, not internal control types.
