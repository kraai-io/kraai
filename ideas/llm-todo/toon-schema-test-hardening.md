# Toon Schema Test Hardening

## Why this matters

`toon-schema` is effectively a compiler front-end for tool definitions. It deserves stronger tests than string containment checks and a few compile-fail fixtures.

## Current gap

The crate already has useful tests, but many assertions are still substring-based. That makes full-schema regressions and diagnostic quality regressions easier to miss.

## Goal

Upgrade `toon-schema` tests so formatting, diagnostics, and example validation behavior are all hard to regress accidentally.

## Plan

1. Add snapshot tests for full rendered TOON schemas.
2. Add snapshot tests for compiler diagnostics from representative failure cases.
3. Expand the compile-fail corpus around nested structs, ranges, defaults, and serde attributes.
4. Add property-style tests for example encoding and parsing invariants where they are easy to express.
5. Add one canonical fixture folder that future tool schema features must update deliberately.

## Milestones

1. Full-output snapshots.
2. Diagnostic snapshots.
3. Expanded compile-fail suite.
4. Property-style validation checks.

## Validation

1. Run the suite after a no-op refactor and ensure snapshots stay stable.
2. Intentionally change formatting to verify snapshots catch it.
3. Verify failure messages stay readable and targeted.

## Risks

Snapshot suites can become noisy. Keep snapshots small, intentional, and focused on one concept per fixture.
