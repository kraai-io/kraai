# Add `edit_file` Tool and Refine Risk Levels

## Summary

Implement a new Rust tool crate, `tool-edit-file`, that performs deterministic exact-text edits against a single file. The tool is patch-like rather than write-like: it applies one or more exact `old_text -> new_text` replacements, fails on ambiguity, and writes only after full validation. It also supports explicit file creation through a `create` flag.

At the same time, replace the current coarse `RiskLevel::OutsideWorkspace` model with distinct outside-workspace read and write levels. The resulting approval behavior stays conservative by default: workspace reads remain auto-approvable, while all outside-workspace reads and writes require approval.

## Public API / Interface Changes

### New tool

Add a new workspace member and runtime registration:

- New crate: `crates/tools/tool-edit-file`
- New runtime registration in `crates/agent-runtime/src/lib.rs`
- New workspace dependency/member entries in root `Cargo.toml`

Tool name:

- Schema/tool id: `edit_file`
- Do not copy the existing `ReadFileTool`/`read_files` naming inconsistency into the new tool.

### `edit_file` schema

Use a single-file patch model:

```json
{
  "path": "relative/or/absolute/path",
  "create": false,
  "edits": [
    {
      "old_text": "exact existing snippet",
      "new_text": "replacement snippet"
    }
  ]
}
```

Rules:

- `path`: required
- `create`: optional, default `false`
- `edits`: required, min length 1
- Non-create mode:
  - target file must already exist
  - each `old_text` must match exactly once in the current file contents
  - if any edit has zero matches or multiple matches, fail the entire tool call
  - apply edits in declared order against an in-memory buffer after validation
- Create mode:
  - target file must not already exist
  - parent directory must already exist
  - require exactly one edit
  - require that edit’s `old_text == ""`
  - created file contents are that edit’s `new_text`
- No directory creation
- No append-only mode
- No line-range editing
- No multi-file edits in one call

### Tool success output

Keep success output minimal:

```json
{ "success": true }
```

Error output remains the existing `ToolOutput::error(...)` shape.

### Risk model change

Update `crates/types/src/lib.rs` to:

```rust
pub enum RiskLevel {
    ReadOnlyWorkspace = 0,
    UndoableWorkspaceWrite = 1,
    NonUndoableWorkspaceWrite = 2,
    ReadOnlyOutsideWorkspace = 3,
    WriteOutsideWorkspace = 4,
}
```

String forms:

- `read_only_workspace`
- `undoable_workspace_write`
- `non_undoable_workspace_write`
- `read_only_outside_workspace`
- `write_outside_workspace`

Rationale locked from discussion:

- outside reads and writes must be distinct
- outside writes do not need undoable vs non-undoable subtypes

### Approval policy defaults

Keep `default_autonomy_threshold()` at `ReadOnlyWorkspace`.

Implications:

- workspace reads: auto-approvable
- workspace writes: approval required unless a tool explicitly lowers/changes policy later
- outside reads: approval required
- outside writes: approval required

For `edit_file` specifically:

- workspace edits: `risk = UndoableWorkspaceWrite`, `policy = AlwaysAsk`
- outside-workspace edits: `risk = WriteOutsideWorkspace`, `policy = AlwaysAsk`

For existing read tools:

- workspace reads remain `ReadOnlyWorkspace`
- outside reads become `ReadOnlyOutsideWorkspace`

## Implementation Details

### `tool-edit-file` behavior

Implement in the same style as existing tool crates:

- `Tool` impl with `name`, `schema`, `assess`, `call`, `describe`
- use `ToonSchema`
- use `resolve_tool_path(...)`

Assessment rules:

- if `path` resolves inside workspace:
  - risk `UndoableWorkspaceWrite`
  - reason `Edits workspace file ...` or `Creates workspace file ...`
- if `path` resolves outside workspace:
  - risk `WriteOutsideWorkspace`
  - reason `Edits file outside workspace ...` or `Creates file outside workspace ...`
- policy always `AlwaysAsk`

Call semantics:

1. Parse args.
2. Resolve path.
3. Branch on `create`.
4. Read current contents when not creating.
5. Validate all edits first against the original/current in-memory content.
6. Reject duplicate/ambiguous matches per edit.
7. Apply edits to a buffer.
8. Write the final contents once.
9. Return `{ success: true }`.

Use normal filesystem writes; treat this as an undoable workspace write because it mutates a single file in-place and is conceptually recoverable by a future edit tool call. Do not introduce backup files in v1.

### `tool-core` adjustments

Refactor shared path/risk helpers so they are no longer read-only specific.

Changes:

- `ResolvedToolPath::risk()` should be removed or narrowed, since risk now depends on operation kind
- replace `assess_read_only_path(...)` with operation-specific helpers, for example:
  - `assess_read_path(...)`
  - `assess_write_path(...)`
- helpers should map workspace/outside-workspace to the new enum values

### Existing tool updates

Update `tool-read-file`, `tool-list-files`, and `tool-search-files`:

- outside path assessments must return `ReadOnlyOutsideWorkspace`
- invalid-args fallback risk should also use the correct outside read level for read tools, not the old coarse variant
- keep their current approval policy behavior otherwise

### UI / bindings impact

Because risk levels are surfaced as strings:

- `crates/agent-ts-bindings/src/lib.rs` likely needs no structural change if it already forwards strings, but regenerated bindings are required after any bindings-facing Rust changes
- desktop app text display in `apps/agent-desktop/src/App.tsx` can remain as-is; it already renders arbitrary risk strings via `replaceAll("_", " ")`

## Test Cases and Scenarios

### New `tool-edit-file` tests

Add unit tests covering:

- edits a workspace file with one exact replacement
- applies multiple edits in one call when each match is unique
- fails when an `old_text` does not exist
- fails when an `old_text` matches multiple times
- validates all edits before writing anything
- create mode creates a missing file when `create=true` and `old_text=""`
- create mode fails if file already exists
- create mode fails if parent directory is missing
- path traversal resolving outside workspace returns outside-write assessment
- `describe()` produces a readable summary

### Updated risk tests

Adjust existing tool tests to assert:

- workspace read tools still assess as `ReadOnlyWorkspace`
- outside read tools now assess as `ReadOnlyOutsideWorkspace`

Add/adjust `tool-core` tests for any new helper functions and risk mapping.

### Integration sanity

Add or update runtime-level assertions, if present, that confirm:

- `edit_file` is registered in `ToolManager`
- approval-required flow still triggers for write tools
- tool result formatting remains valid with `{ "success": true }`

## Validation / Checks

After implementation, run:

1. `just lint`
2. `cargo nextest run`
3. `cargo clippy --all-targets -- -D warnings`

`just typecheck-desktop` is not required unless TypeScript files change.
`just build-bindings-debug` is required if `crates/agent-ts-bindings` outputs need regeneration or checked-in generated artifacts change.

## Assumptions and Defaults

- Tool id will be `edit_file`.
- Success payload is exactly `{ "success": true }`.
- File creation is allowed only with `create=true`.
- Create mode is explicit and narrow: one edit only, `old_text == ""`, no directory creation.
- Ambiguous matches are treated as hard failures.
- Multi-edit calls are atomic: validate first, write once.
- Outside-workspace writes are allowed only through explicit approval, not auto-approved and not blocked outright.
- Existing inconsistent naming like `read_files` is treated as legacy and not copied into the new tool.
