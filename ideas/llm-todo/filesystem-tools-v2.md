# Filesystem Tools V2

## Why this matters

The existing filesystem tools are useful but still shaped like prototype primitives. As agent loops get longer, they will need bounded outputs, richer metadata, and more composable result shapes.

## Current gap

`read_files` reads full files into memory and returns numbered strings without file-path metadata per result. `search_files` hardcodes a match cap with no byte budget or path filtering. `list_files` returns shallow entries but little metadata.

## Goal

Upgrade file tools into a coherent bounded toolkit that scales better in agent loops.

## Plan

1. Define a shared filesystem result envelope in `tool-core`.
2. Add per-tool output fields for:
   - resolved path
   - truncation status
   - byte counts
   - file type metadata
3. Add bounded read controls such as `max_bytes`, `offset`, and maybe `line_range`.
4. Add search controls such as `glob`, `include_hidden`, `max_matches`, and `max_total_bytes`.
5. Add richer list metadata such as size and modified time.
6. Make output ordering deterministic across all tools.

## Milestones

1. Shared result envelope.
2. `read_files` bounds and metadata.
3. `search_files` filters and byte budgeting.
4. `list_files` metadata enrichment.

## Validation

1. Tests for large files and truncation accounting.
2. Tests for binary files and ignored files.
3. Tests proving deterministic ordering across platforms.

## Risks

More capability increases risk surface. Keep policy and risk assessment aligned with the new arguments, especially when broadening search scope.
