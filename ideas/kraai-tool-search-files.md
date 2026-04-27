# kraai-tool-search-files review

Scope: `crates/tools/kraai-tool-search-files`, with limited checks against `kraai-tool-core` and sibling file tools for path and tool-contract consistency.

## Findings

### High: approved workspace searches can follow symlinks outside the workspace

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:78-90,109-112,145-155`; path assessment helper at `crates/tools/kraai-tool-core/src/lib.rs:364-369`.
- Impact: `assess` classifies a path through `resolve_tool_path`, which canonicalizes the candidate and can mark `workspace/outside-link` as `ReadOnlyOutsideWorkspace`. However `call` does not enforce that assessment result; it always proceeds after resolving the path. For a directory search, `ignore::WalkBuilder` follows the tool path when that path itself is a symlink to a directory, then searches the target. That means a model can request a workspace-looking symlink path, get an outside-workspace assessment, and still read/search outside files if the surrounding execution layer ever calls the tool after approval, misconfiguration, or replay.
- Suggested fix: make execution enforce the same boundary decision as assessment for autonomous tools, either centrally in the tool runner or locally before filesystem access. For this crate, consider returning a structured error when `!resolved.is_within_workspace()` unless the tool call has an explicit approval token/capability. Add a Unix test that creates `workspace/outside-link -> outside_dir`, calls `assess`, then verifies `call` cannot search the outside file without outside-workspace approval.

### Medium: one unreadable directory entry aborts the whole search

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:155-170`.
- Impact: `for entry in builder.build()` immediately returns `Err` on any walk error (`let entry = entry?`). Large workspaces often contain transient, permission-denied, broken symlink, or deleted-while-walking paths. A single bad entry prevents returning valid matches found later in the tree, making the search tool less reliable under normal project churn and under load.
- Suggested fix: treat walk errors like non-fatal skipped entries and include skipped/error counts in the output. For example, collect `walk_errors: Vec<String>` up to a small cap, continue on per-entry errors, and only fail for setup-level errors. Add a test with an unreadable directory on Unix, or a broken symlink entry, to lock in best-effort behavior.

### Medium: non-UTF-8 handling depends on matching an error string

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:165-168`.
- Impact: `error.to_string().contains("invalid utf-8")` is brittle. It depends on the exact display text from `grep-searcher`/sink internals and can change across dependency versions. If the text changes, a single non-UTF-8 file aborts the directory search instead of being skipped, despite the existing test intending that behavior (`src/lib.rs:400-435`).
- Suggested fix: avoid string inspection. Prefer a sink/search mode that handles arbitrary bytes, use a typed error path if the grep crates expose one, or explicitly read/search text files with a shared UTF-8 reader that returns a known error enum. If the tool intentionally only emits UTF-8 lines, model the skip reason explicitly and add tests for invalid UTF-8 with and without NUL bytes.

### Medium: synchronous filesystem search runs inside an async tool call

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:88-137,145-213`.
- Impact: `call` is async but performs all metadata, directory walking, and grep searching synchronously on the executor thread. A broad search over a large workspace can block other runtime tasks, delay streaming/session work, and create unpredictable latency. This conflicts with the project priority to keep behavior predictable under load.
- Suggested fix: run blocking filesystem work via `tokio::task::spawn_blocking` or move file tools behind a bounded blocking pool. Keep the result type owned and serializable so the blocking closure does not borrow `ToolContext`. Add a stress/regression test or runtime-level test that concurrent lightweight tasks are not starved by a large search.

### Medium: output `match_count` is the returned count, not the total match count

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:48-50,128-133,191-206`.
- Impact: when results are truncated, `match_count` is set to `state.matches.len()`, which is always at most `MAX_MATCHES`. The field name reads like total matches found, but after truncation it only reports returned matches. Agents cannot tell whether there were 101 matches or 10,000, which hurts planning and token-efficient follow-up searches.
- Suggested fix: either rename the field to `returned_match_count` or keep scanning after the output cap to count total matches up to a second inexpensive cap. A useful contract would be `{ returned_match_count, total_match_count: Option<usize>, truncated }`. Update `truncates_after_maximum_matches` (`src/lib.rs:460-490`) to assert the chosen semantics.

### Low: result order is filesystem-dependent

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:155-174`.
- Impact: `ignore::WalkBuilder` traversal order is not normalized by this crate. Search results can vary across filesystems/platforms or even between runs after directory changes. That makes tool output harder to test, replay, and compare in agent traces.
- Suggested fix: if deterministic output matters more than streaming first results, collect paths and sort them before searching. If first-result latency matters more, document nondeterministic order and make tests avoid depending on order. Given agent reproducibility, a sorted traversal is probably the better default for small/medium trees.

### Low: search paths in output are absolute display strings

- Location: `crates/tools/kraai-tool-search-files/src/lib.rs:47,55,128-130,189-199`.
- Impact: every match repeats an absolute path, which wastes tokens and leaks host-specific temp/home prefixes into model context. Sibling tools also use display paths, but this crate amplifies the cost because each match carries a path. In a 100-match result set, repeated workspace prefixes can dominate the response.
- Suggested fix: return workspace-relative paths when `resolved.is_within_workspace()` and include a single `root` or `searched_path` field for context. For outside-workspace approved reads, keep absolute paths or add `path_kind: "outside_workspace"`. Add tests for relative output in workspace-root, nested-directory, and single-file searches.

### Low: the crate carries unused dependencies

- Location: `crates/tools/kraai-tool-search-files/Cargo.toml:20-22`.
- Impact: `serde_json`, `tokio`, and `toon-format` are listed as normal dependencies, but the library code only uses `tokio` in tests and does not appear to use `serde_json` or `toon-format` directly. This increases compile surface and makes dependency ownership less clear. The same pattern exists in some sibling crates, but it is still worth cleaning before it propagates.
- Suggested fix: move `tokio` to `[dev-dependencies]` if workspace policy allows, and remove `serde_json`/`toon-format` unless the macro expansion requires direct dependencies. Verify with `cargo check -p kraai-tool-search-files` after each removal.

## Missing or weak tests

- No symlink execution test: existing assessment tests cover parent traversal (`src/lib.rs:553-576`) but not call-time behavior for symlink escapes.
- No unreadable/broken-entry test: directory traversal failure behavior at `src/lib.rs:155-170` is untested.
- No deterministic ordering test or explicit nondeterminism contract.
- No test for output path shape/token budget. Current tests only assert suffixes for some paths (`src/lib.rs:323-328,387-392,424-429`), so absolute-path churn can slip through unnoticed.
- No direct schema/description test. `describe` (`src/lib.rs:139-142`) includes raw regex text and could become noisy for long patterns; a max length or escaping policy is not covered.

## Refactor opportunities

- Extract a shared file-search service from the tool wrapper. `SearchFilesTool::call` should mostly resolve/assess args and serialize output; traversal, matching, truncation, and skip accounting can live in a small testable module.
- Replace `Box<dyn std::error::Error + Send + Sync>` in `search_directory`/`search_file` (`src/lib.rs:149,183`) with a local error enum. That would remove string matching, make skip-vs-fatal decisions explicit, and improve tests.
- Introduce a common blocking-filesystem pattern for read/list/search tools. `kraai-tool-read-file`, `kraai-tool-list-files`, and this crate all do synchronous filesystem work behind async trait methods.

## Test command

Not run. This task requested an ideas report only and no source changes outside `ideas/kraai-tool-search-files.md`.
