# kraai-tool-read-file findings

Scope: `crates/tools/kraai-tool-read-file`.

## Findings

### High: batched reads fail atomically and discard successful earlier reads

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:77-91`
- Issue: `call` reads files sequentially, but returns `ToolCallResult::error(error)` on the first failure at `src/lib.rs:82-85`. Any successfully read files before that point are discarded from both the output and `file_reads` state deltas.
- Impact: A request such as `["a.txt", "missing.txt", "b.txt"]` loses the useful content and read-state refresh for `a.txt`. This makes the tool brittle under partial filesystem failures and makes large multi-file reads unnecessarily all-or-nothing.
- Suggested fix: Return per-file results instead of a single top-level error for the whole batch. For example, output entries like `{ path, success, contents?, error? }`, emit deltas for successful reads only, and only mark the whole tool call as failed if the framework requires that for all-error batches. If the public output shape must remain `files: Vec<String>` temporarily, add a parallel `errors` field and migrate callers.
- Tests to add: A mixed success/failure batch should preserve successful file contents and emit deltas for successful reads. A fully failing batch should have a predictable error shape.

### Medium: output omits paths, making multi-file results ambiguous

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:33-36`, `src/lib.rs:86-90`
- Issue: `ReadFileToolOutput` is only `files: Vec<String>`, and each entry is just numbered contents. For multi-file reads, callers must rely on positional correspondence to the request to know which content belongs to which path.
- Impact: This is fragile for agents and UIs, especially once partial success, duplicate paths, canonicalized paths, or reordered/concurrent reads are introduced. It also inflates the chance of editing the wrong file after reading several files.
- Suggested fix: Return structured entries with at least the requested path and resolved path, e.g. `files: Vec<ReadFileEntry> { requested_path, path, contents }`. Consider including `sha256` if exposing it helps debug edit freshness, though the state delta should remain the source of truth.
- Tests to add: Multi-file output should include the resolved path for each entry and preserve request order.

### Medium: synchronous filesystem reads run inside an async tool method

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:77-91`, via `kraai_tool_core::read_text_file` at `crates/tools/kraai-tool-core/src/lib.rs:398-420`
- Issue: `ReadFileTool::call` is async but performs blocking `std::fs::read_to_string` through `read_text_file`.
- Impact: Large files, slow network mounts, or many concurrent read tool calls can block Tokio worker threads. This conflicts with the repo priority to keep behavior predictable under load.
- Suggested fix: Either make tool calls explicitly blocking and run them through a dedicated blocking pool in the runtime, or switch the shared text-read helper to an async variant using `tokio::fs` for async tools. If hashing stays CPU-bound for large files, consider `spawn_blocking` for read+hash as one unit.
- Tests to add: This is hard to unit test directly; add a runtime-level concurrency regression test if the runtime gets a blocking-pool abstraction.

### Medium: no size limit or truncation strategy for file contents

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:81-87`; shared reader at `crates/tools/kraai-tool-core/src/lib.rs:411-413`
- Issue: The tool reads the full file into memory and returns the full numbered contents with no byte, line, or token budget.
- Impact: A single large text file can consume substantial memory, produce huge tool results, and overload downstream model context. The sibling `search_files` tool has an explicit cap (`crates/tools/kraai-tool-search-files/src/lib.rs:17`, `src/lib.rs:191-206`); `read_files` should have an equally deliberate policy.
- Suggested fix: Add optional `start_line`/`end_line` or `max_bytes` arguments, plus a default maximum with an explicit `truncated` indicator. For full-file reads, require an explicit opt-in once the file exceeds the threshold.
- Tests to add: Large file reads should truncate predictably and report truncation metadata. Line range reads should preserve one-based line numbering.

### Low: assessment logic duplicates the shared read-path helper

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:50-75`; shared helper at `crates/tools/kraai-tool-core/src/lib.rs:422-444`
- Issue: `ReadFileTool::assess` manually repeats path resolution, risk escalation, and reason formatting. Sibling tools use `assess_read_path` for single paths, but there is no shared helper for batches.
- Impact: More places need to be updated if path risk policy changes. The manual version is already slightly different because it aggregates multiple reasons and escalates risk across the batch.
- Suggested fix: Add a shared `assess_read_paths` helper in `kraai-tool-core` that accepts an iterator and returns the max risk plus all reasons. Then use it here and in future multi-path read tools.
- Tests to add: Mixed workspace/outside batches should produce `ReadOnlyOutsideWorkspace`, preserve all reasons, and keep `AutonomousUpTo(ReadOnlyWorkspace)`.

### Low: tests use hand-rolled temp directories and can leak on panic

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:128-143`, cleanup calls such as `src/lib.rs:175`, `src/lib.rs:207`, `src/lib.rs:242-243`
- Issue: Tests create temp dirs manually and clean them up at the end. If a test panics before cleanup, the directory remains. This pattern is duplicated across tool crates.
- Impact: Local and CI temp directories can accumulate stale files, and repeated helper duplication makes tests noisier than necessary.
- Suggested fix: Use `tempfile::TempDir` or introduce a shared test helper crate/module for tool test contexts. This should also centralize `ToolCallGlobalConfig`, `ToolContext`, and argument helper setup.
- Tests to add: No behavior test needed; this is a test infrastructure refactor.

### Low: missing tests for native TOON parsing and schema boundary behavior

- Location: `crates/tools/kraai-tool-read-file/src/lib.rs:101-319`
- Issue: The crate tests direct Rust args but does not test that native TOON input decodes and validates through `ToolManager::prepare_tool`. `edit_file` has this style of coverage at `crates/tools/kraai-tool-edit-file/src/lib.rs:1053-1079`.
- Impact: Schema/parser regressions in the actual model-facing syntax can slip through even though direct struct construction tests pass. This matters for `files: Vec<String>` because list syntax is model-facing and easy to regress.
- Suggested fix: Add tests that decode valid native TOON with one and multiple files, then prepare `read_files` through `ToolManager`. Add a negative test for missing `files` or an empty `files` list if the schema validation layer is expected to enforce `min = 1`.

### Low: manifest includes unused direct dependencies

- Location: `crates/tools/kraai-tool-read-file/Cargo.toml:16-17`
- Issue: `serde_json` and `toon-format` are listed as direct dependencies but are not used by the crate or its current tests. `serde_json` is used indirectly through `kraai-tool-core::file_read_refresh_delta`, but that does not require a direct dependency here.
- Impact: Minor dependency hygiene issue. It increases compile graph surface and makes it harder to see what the crate actually needs.
- Suggested fix: Remove unused direct dependencies, or move `toon-format` to `dev-dependencies` if native TOON parsing tests are added.

## Architecture notes

- The source file is still small, so it is not a god file yet. The bigger maintainability issue is duplicated tool-test scaffolding across `read_file`, `open_file`, `search_files`, and `edit_file`.
- Several findings point at shared tool-core abstractions rather than only this crate: batched path assessment, async/blocking filesystem policy, output conventions for path-bearing tool results, and tempdir-based test fixtures. Fixing those centrally would reduce future drift.
