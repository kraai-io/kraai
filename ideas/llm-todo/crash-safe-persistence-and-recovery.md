# Crash-Safe Persistence And Recovery

## Why this matters

The store already uses atomic writes for sessions, but message persistence and startup recovery are still fragile in the face of torn writes or malformed files.

## Current gap

Messages are written directly to their final path. Session loading fails hard on malformed `sessions.json`. There is no quarantine or repair path for partially written files.

## Goal

Make storage resilient to partial writes, corrupted files, and interrupted process shutdown.

## Plan

1. Use temp-file-plus-rename writes for messages as well as sessions.
2. Add a small versioned file envelope to sessions and messages so future migrations are explicit.
3. On load failure, quarantine unreadable files into a recovery folder instead of aborting startup.
4. Add a repair report that lists what was skipped, quarantined, or reconstructed.
5. Expose a maintenance task that can rebuild secondary indexes or manifests after recovery.

## Milestones

1. Atomic message writes.
2. Versioned envelopes.
3. Quarantine and repair reporting.
4. Recovery command or maintenance entrypoint.

## Validation

1. Failure-injection tests for malformed JSON, truncated files, and interrupted renames.
2. Tests proving startup can continue with partial damage.
3. Tests proving unrecoverable files do not silently disappear without a report.

## Risks

Recovery logic can become too magical. Prefer preserving bad files verbatim in quarantine and telling the operator exactly what happened.
