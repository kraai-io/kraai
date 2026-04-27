# kraai-tool-close-file review

Scope: `crates/tools/kraai-tool-close-file`, with cross-crate checks where the tool state delta contract is consumed.

## Findings

### Medium: `close_file` reports success even when it did not close anything

- Location: `crates/tools/kraai-tool-close-file/src/lib.rs:63-75`; state consumer at `crates/kraai-agent/src/tool_state.rs:147-166`.
- Impact: `call` resolves the requested path, returns `success: true`, and emits a `opened_files.close` delta unconditionally. If the path is not currently open, misspelled, differently normalized, or already closed, the user and model still see a successful close result. The only test explicitly locks this in by closing `missing.txt` against an empty snapshot (`src/lib.rs:98-118`). This makes state transitions harder to reason about and can hide no-op tool calls during session recovery/debugging.
- Suggested fix: read `ctx.tool_state_snapshot` and distinguish `closed` from `not_open`. Either return an error/no-op output when the path is absent, or return structured output such as `{ "closed": false, "reason": "not_open", "path": ... }` and emit no delta for no-ops. Add tests for open path, absent path, already closed path, and repeated close idempotency.

### Medium: opened-file state is duplicated as stringly typed JSON across crates

- Location: constants and delta payload in `crates/tools/kraai-tool-close-file/src/lib.rs:11-12,70-74`; matching parser constants in `crates/kraai-agent/src/tool_state.rs:11-13,137-166`; equivalent open-file constants in `crates/tools/kraai-tool-open-file/src/lib.rs:12-13,75-82`.
- Impact: the namespace (`"opened_files"`), operations (`"open"`, `"close"`), and payload shape (`{ "path": String }`) are hand-duplicated in the producer crates and consumer. A typo or future shape change silently stops working because `apply_opened_file_delta` ignores unknown operations and malformed payloads (`tool_state.rs:137-157`). This is especially risky for persisted history replay, where a bad delta can make restarted sessions inject the wrong context with no warning.
- Suggested fix: move opened-file delta construction and parsing into shared code, likely `kraai-tool-core` or a small state-contract module. Expose typed constructors such as `opened_file_open_delta(path)` and `opened_file_close_delta(path)`, plus a parser that returns an explicit error/warning for malformed known deltas. Update open/close tools and agent state replay to use that contract.

### Medium: close path matching depends on exact displayed path strings

- Location: `crates/tools/kraai-tool-close-file/src/lib.rs:64,73`; open delta uses `read.path().display().to_string()` in `crates/tools/kraai-tool-open-file/src/lib.rs:70-82`; close removal uses exact string equality in `crates/kraai-agent/src/tool_state.rs:154-156`.
- Impact: closing only works if the resolved display string exactly matches the stored open path. The current `resolve_tool_path` normalization probably makes common relative forms match, but the contract is implicit and untested. Edge cases such as symlink paths, workspace path spelling changes, non-canonical workspace roots, platform-specific path display differences, or a future change to make open store canonical paths could make `close_file` return success while leaving the file pinned.
- Suggested fix: centralize path identity for opened files. Decide whether opened files are keyed by normalized lexical path or canonical path, document it in the shared delta API, and test equivalent forms (`src/../src/lib.rs`, absolute vs relative, and symlink escape behavior on Unix). If canonical identity is wanted, store canonical paths where possible and keep a stable fallback for missing files.

### Low: tests barely cover the output contract and do not validate snapshot effects

- Location: `crates/tools/kraai-tool-close-file/src/lib.rs:83-119`.
- Impact: the only test checks risk and delta count/operation. It does not assert output shape, emitted payload path, outside-workspace assessment, `describe`, schema basics, or the end-to-end effect of applying the delta to an `opened_files` snapshot. This leaves the actual contract with `kraai-agent/src/tool_state.rs` mostly unprotected.
- Suggested fix: add focused tests for a successful close against a snapshot containing the resolved path, no-op/absent-path behavior after that policy is decided, payload path equality, outside-workspace risk, and replay through `resolve_snapshot_from_history` or a shared parser. Also assert the success output data, not only the delta vector.

### Low: temporary test paths are hard-coded and unrealistic

- Location: `crates/tools/kraai-tool-close-file/src/lib.rs:101`.
- Impact: the test uses `/tmp/workspace` without creating it. That happens to work because `close_file` does not touch the filesystem, but it avoids exercising the real workspace path resolution paths used by sibling tool tests. It also makes it easy to accidentally keep passing if `resolve_tool_path` behavior changes for missing workspace roots.
- Suggested fix: copy the small temp-dir helper pattern from `kraai-tool-open-file` or `kraai-tool-read-file`, create the workspace directory, and clean it up. If no filesystem access is intentionally required, add a test name/assertion that makes the no-filesystem contract explicit.

## Refactor opportunities

- `CloseFileToolOutput { success: bool, path: String }` is too coarse for state tools. Prefer a status enum or fields like `closed: bool` and `reason: Option<String>` so callers can distinguish a real mutation from a no-op.
- `ToolContext::tool_state_snapshot` is currently unused in this crate despite being the only way to validate whether a close operation is meaningful. Using it would make close behavior more predictable and would justify the context dependency.
- Consider extracting an `opened_files` helper alongside `file_read_refresh_delta` in `kraai-tool-core`. Open, close, and agent replay should not each know the raw JSON strings.

## Test command

Not run. This task requested an ideas report only and no source changes outside `ideas/kraai-tool-close-file.md`.
