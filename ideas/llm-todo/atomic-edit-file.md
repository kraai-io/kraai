# Atomic Edit File

## Why this matters

`edit_file` is already deterministic, but it still writes directly to target files with no concurrency precondition, no preview mode, and no atomic rename. That is risky in multi-agent or multi-process workflows.

## Current gap

The tool applies exact-text edits in memory and writes the result straight back to disk. If another writer changes the file between read and write, the tool has no way to detect it.

## Goal

Make `edit_file` safe enough for concurrent and automated agent workflows.

## Plan

1. Add optional preconditions such as:
   - expected file hash
   - expected modified time
2. Add a `dry_run` mode that returns a structured diff or patch preview.
3. Switch write paths to temp-file-plus-rename for atomic replacement.
4. Split create and edit behavior if that simplifies validation and risk policy.
5. Add clearer error variants for precondition failure, ambiguous edits, and atomic write failures.

## Milestones

1. Atomic writes.
2. Preconditions.
3. Dry-run and diff response.
4. Optional split into `create_file` and `edit_file`.

## Validation

1. Tests for concurrent modification detection.
2. Tests proving failed edits do not partially modify the file.
3. Tests for dry-run output and ambiguous multi-edit sequencing.

## Risks

Do not add enough flexibility that the tool becomes a generic patch engine without corresponding policy controls. Keep the first version narrow and predictable.
