# kraai-tool-open-file review

Scope: `crates/tools/kraai-tool-open-file`, with cross-crate checks where its deltas are consumed by agent state and runtime tests.

## Findings

### High: opening files can create unbounded future prompt injection

- Location: `crates/tools/kraai-tool-open-file/src/lib.rs:64-82`; prompt injection consumer at `crates/kraai-agent/src/tool_state.rs:47-78`.
- Impact: `open_file` reads the full file only to validate/read-hash it, then pins the path into `opened_files`. On every future turn, `refresh_and_render_system_prompt` re-reads every opened file and injects the complete contents with line numbers. There is no file size limit, total opened-file budget, count limit, binary/huge-file guard beyond UTF-8 readability, or truncation signal. A single large text file, or many moderate files, can permanently inflate prompts, slow every turn, and push sessions over model context limits. Because the open result returns only `{ success, path }`, the caller also gets no warning that future turns will carry a large hidden token cost.
- Suggested fix: add explicit budgets to the opened-file workflow. At minimum, enforce max file bytes per opened file, max total opened bytes/tokens, and max opened path count before emitting the open delta. Return structured metadata such as byte count, line count, and whether content will be truncated. The render path should enforce the same budgets during refresh, because files can grow after being opened.

### Medium: `open_file` allows pinning outside-workspace files after approval

- Location: assessment at `crates/tools/kraai-tool-open-file/src/lib.rs:53-61`; execution at `src/lib.rs:64-82`; consumer re-read at `crates/kraai-agent/src/tool_state.rs:61-74`.
- Impact: outside-workspace paths are classified as `ReadOnlyOutsideWorkspace`, but the tool still supports them if policy permits execution. Once opened, the absolute path is stored and later re-read by `refresh_and_render_system_prompt` without workspace context or a fresh assessment. That means an approved outside-workspace read becomes a persistent prompt injection source across future turns, including after session restart/history replay. This is a larger blast radius than an ordinary one-shot read.
- Suggested fix: make persistence of outside-workspace reads a separate policy decision. Options: reject outside-workspace `open_file` entirely, require an explicit higher-risk policy for persistent outside-workspace context, or store risk metadata with opened paths and re-check it before refresh. Add tests for parent traversal and absolute outside paths that verify both assessment and whether a delta is emitted.

### Medium: opened-file state is duplicated as stringly typed JSON across crates

- Location: constants and delta payload in `crates/tools/kraai-tool-open-file/src/lib.rs:12-13,75-82`; close-file duplicate at `crates/tools/kraai-tool-close-file/src/lib.rs:11-12,70-74`; parser in `crates/kraai-agent/src/tool_state.rs:11-13,137-166`.
- Impact: the namespace (`"opened_files"`), operation names (`"open"`, `"close"`), and payload shape (`{ "path": String }`) are repeated manually in producer crates and the consumer. A typo or future shape change silently breaks replay because unknown operations and malformed payloads are ignored. This is particularly risky for persisted history and session restart behavior, where state needs to be predictable under failures.
- Suggested fix: move opened-file delta constructors and parsers into shared code, likely `kraai-tool-core` or a small state-contract module. Expose typed helpers such as `opened_file_open_delta(path)` / `opened_file_close_delta(path)` and one parser used by the agent. Prefer explicit errors or diagnostics for malformed known deltas over silent drops.

### Medium: path identity is implicit and may drift between open, close, refresh, and edit guards

- Location: open stores `read.path().display().to_string()` at `crates/tools/kraai-tool-open-file/src/lib.rs:73,79`; close removes by exact string equality at `crates/kraai-agent/src/tool_state.rs:154-156`; refresh re-reads `Path::new(&path)` at `tool_state.rs:61-74`.
- Impact: the opened-file key is a displayed path string from `resolve_tool_path`/`read_text_file`, not a documented identity type. Common lexical normalization is handled today, but equivalent paths involving symlinks, differently spelled workspace roots, case-sensitive/case-insensitive filesystems, or future canonicalization changes can diverge. The result can be duplicate pinned entries, `close_file` reporting success while leaving a file open, or edit/read freshness checks using a different path string from the one stored by open.
- Suggested fix: define opened-file path identity in one place. Decide whether keys are lexical normalized paths or canonical paths, store that representation consistently, and test absolute vs relative, `src/../src/lib.rs`, and Unix symlink cases. Avoid deriving state keys from `display().to_string()` at each call site.

### Low: unit test only checks the happy path shallowly

- Location: `crates/tools/kraai-tool-open-file/src/lib.rs:91-166`.
- Impact: the only unit test verifies workspace risk, success path, delta count, the first delta operation, and the second delta namespace. It does not assert the opened-file delta namespace, payload path, file-read refresh operation/hash, outside-workspace assessment, missing-file error, directory error, duplicate-open/idempotency behavior after replay, `describe`, or schema basics. Runtime tests cover one next-turn edit flow (`crates/kraai-runtime/src/runtime/tests/workspace.rs:153-265`), but not malformed/edge delta contracts or prompt-size behavior.
- Suggested fix: add focused unit tests for payload shape and state replay, parent traversal/outside-workspace assessment, missing and directory paths, and duplicate opens. If budgets are added, test refusal/truncation boundaries. For contract tests, use the shared opened-file helper rather than reasserting raw strings in every crate.

### Low: synchronous filesystem reads run inside an async tool call

- Location: `crates/tools/kraai-tool-open-file/src/lib.rs:64-68`; shared read helper uses `std::fs::read_to_string` at `crates/tools/kraai-tool-core/src/lib.rs:398-419`.
- Impact: `call` is async, but file reads and hashing are synchronous. For small files this is fine, but opening large files or slow network-mounted paths can block the async runtime worker, and the opened-file refresh path also uses synchronous reads. This gets worse if users open multiple large files and every subsequent turn refreshes them.
- Suggested fix: either move filesystem work behind `tokio::task::spawn_blocking`/async fs in the tool layer, or make the runtime execute tool calls on a blocking pool. Combine this with file-size budgets so the blocking window is bounded.

### Low: crate manifest keeps test-only and unused dependencies in normal dependencies

- Location: `crates/tools/kraai-tool-open-file/Cargo.toml:14,17`.
- Impact: `tokio` is only used by the crate's unit test (`src/lib.rs:135`), but it is declared under `[dependencies]`. `kraai-workspace-hack` is declared but not referenced by source in this crate. This slightly inflates the normal dependency surface and obscures what production code actually needs.
- Suggested fix: move `tokio` to `[dev-dependencies]` if workspace/test setup allows it. Remove `kraai-workspace-hack` from this crate if it is not required for workspace packaging; if it is intentionally present for Nix/workspace reasons, document that convention centrally so it does not look accidental.

## Refactor opportunities

- Extract an `opened_files` contract next to `file_read_refresh_delta` in `kraai-tool-core`. Open, close, and agent replay should share typed constructors, operation constants, and path normalization.
- Introduce an `OpenedFilePath` or `ToolStatePathKey` newtype instead of passing displayed path strings through JSON at every boundary.
- Return richer open output: `opened: bool`, `path`, `bytes`, `sha256`, and possibly `already_open`/`outside_workspace`/`will_truncate` fields. The current `success: true` does not tell the model or UI the operational cost of the state change.
- Consider merging open/close state tests into a shared contract test module. The real behavior is the transition from tool delta to `ToolStateSnapshot` to refreshed prompt, not just the local vector of emitted deltas.

## Test command

Not run. This task requested an ideas report only and explicitly limited source modifications to `ideas/kraai-tool-open-file.md`.
