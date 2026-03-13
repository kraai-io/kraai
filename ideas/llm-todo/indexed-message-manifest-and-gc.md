# Indexed Message Manifest And GC

## Why this matters

The current file-based store is correct but cleanup scales with total data size because it re-scans messages and session trees. That will become a startup and deletion tax as sessions grow.

## Current gap

Startup cleanup walks all sessions and all on-disk messages to compute orphaned message files. Session deletion also re-traverses trees to decide what to delete. There is no persisted ownership index or refcount manifest.

## Goal

Replace full-tree scans with a persisted manifest that makes common cleanup operations proportional to the session or message set being changed.

## Plan

1. Add a manifest keyed by `MessageId` that records:
   - parent id
   - refcount or owning session ids
   - basic status metadata
2. Update the manifest transactionally when:
   - saving a new message
   - changing a session tip
   - deleting a session
3. Use the manifest for orphan detection on normal paths.
4. Keep the current full cleanup as a repair command for older data or manifest corruption.
5. Add simple benchmark fixtures for large session trees to measure startup and delete costs before and after.

## Milestones

1. Build manifest writes in parallel with current behavior.
2. Read manifest for deletion decisions.
3. Use manifest during startup instead of global scans.
4. Add repair/rebuild support.

## Validation

1. Tests for shared ancestry between sessions.
2. Tests proving session deletion only removes unreferenced unique messages.
3. Benchmarks for startup and delete time with large histories.

## Risks

A bad manifest is worse than a slow scan. The plan should include a rebuild path from authoritative message/session files until confidence is high.
