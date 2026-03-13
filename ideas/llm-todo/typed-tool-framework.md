# Typed Tool Framework

## Why this matters

Every tool currently repeats the same serde parsing logic across validation, assessment, description, and execution. That is a maintenance tax and a source of drift.

## Current gap

The file tools all do their own `serde_json::from_value` calls in multiple methods, and `ToolManager::call_tool()` does not itself enforce validation before execution. The framework is usable, but it still feels like a low-level demo layer.

## Goal

Make tool authoring typed by default so a new tool defines its arguments once and gets consistent validation, description, and execution plumbing automatically.

## Plan

1. Add a `TypedTool<Args>` helper trait or extend `toon_tool!` to derive tool adapters.
2. Make parsed arguments flow through:
   - validate
   - assess
   - describe
   - call
   from one canonical parse step.
3. Update `ToolManager` with a `prepare_and_call` path that always validates before execution.
4. Convert existing built-in tools to the typed framework first.
5. Add tests proving malformed args never reach `call()`.

## Milestones

1. Add generic typed adapter.
2. Migrate one read-only tool as a pilot.
3. Migrate the rest of the built-in tools.
4. Remove duplicated parsing code paths.

## Validation

1. Unit tests for parse/validate failure handling.
2. Tests verifying `describe()` and `assess()` use the same typed args as `call()`.
3. Diff the tool schema output before and after migration to ensure compatibility.

## Risks

The abstraction should reduce boilerplate, not hide tool policy. Keep risk assessment logic explicit even if the parsing layer becomes generic.
