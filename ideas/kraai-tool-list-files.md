# kraai-tool-list-files review

Scope: `crates/tools/kraai-tool-list-files`.

The crate is small and readable, but it currently behaves like a raw `std::fs::read_dir` wrapper inside an async tool. The biggest concerns are reliability under large directories, blocking executor threads, and path/symlink behavior that can make safety assessment stale by the time the tool is actually called.

## Findings

### High: unbounded directory output can blow up memory and token usage

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:36`, `crates/tools/kraai-tool-list-files/src/lib.rs:87`, `crates/tools/kraai-tool-list-files/src/lib.rs:109-124`
- Issue: `read_entries` collects every directory entry into a `Vec` and the tool returns all entries with full displayed paths. A large generated directory, repository cache, `node_modules`, or `/tmp`-style target can create a huge JSON result. This is especially risky for an LLM tool because the output is immediately serialized and likely injected into model context.
- Impact: high memory use, slow tool calls, excessive token spend, degraded responsiveness, and possible runtime instability under load.
- Suggested fix: add an explicit limit such as `MAX_ENTRIES`, return `truncated: bool`, `entry_count`, and maybe `returned_count`. Stop reading after the limit. Consider defaulting to directories-first/name-sorted while still bounding memory. If full counts matter, make that optional because counting requires walking the whole directory.
- Suggested tests: create more than the limit and assert `entries.len() == MAX_ENTRIES`, `truncated == true`, and stable ordering of returned entries.

### High: synchronous filesystem work runs directly inside an async tool call

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:67-102`, `crates/tools/kraai-tool-list-files/src/lib.rs:69`, `crates/tools/kraai-tool-list-files/src/lib.rs:110`
- Issue: `call` is `async`, but it executes blocking `std::fs::metadata` and `std::fs::read_dir` work on the async executor thread. This is small for tiny directories, but listing slow network mounts or very large directories can block unrelated runtime work.
- Impact: poor latency and unpredictable behavior under load, especially if multiple tool calls run concurrently.
- Suggested fix: either use `tokio::fs` for metadata/read_dir or wrap the entire listing in `tokio::task::spawn_blocking`. `spawn_blocking` is probably the simpler fit if the implementation keeps using `std::fs::DirEntry` and metadata APIs.
- Suggested tests: unit tests will not catch executor starvation well; add a focused concurrency/regression test only if the runtime has a standard pattern for this. Otherwise document the async blocking policy in `kraai-tool-core` and apply it consistently across file tools.

### Medium: symlink target changes can bypass the assessment made before execution

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:58-65`, `crates/tools/kraai-tool-list-files/src/lib.rs:68-87`, `crates/tools/kraai-tool-core/src/lib.rs:364-369`
- Issue: `assess` uses `assess_read_path`, which resolves/canonicalizes the path at assessment time. `call` resolves the path again and then follows symlinks through `std::fs::metadata` and `read_dir`. If a workspace symlink is swapped after assessment but before execution, the call can list a different target than the one assessed.
- Impact: a path approved as `ReadOnlyWorkspace` could list outside-workspace contents if the filesystem changes between assessment and execution. This is read-only, but still a safety boundary for secrets and host paths.
- Suggested fix: carry the assessed `ResolvedToolPath` into the prepared tool call, or make the tool call re-check `resolved.is_within_workspace()` and enforce that the current risk is still allowed by the policy immediately before filesystem access. Longer term, centralize this in `kraai-tool-core` so all filesystem tools share one assessment/execution path.
- Suggested tests: on Unix, create a symlink inside the workspace, assess while it points inside, swap it to an outside directory before `call`, and assert the execution is blocked or reassessed as outside workspace.

### Medium: output path strings expose absolute host paths and waste tokens

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:35`, `crates/tools/kraai-tool-list-files/src/lib.rs:41-43`, `crates/tools/kraai-tool-list-files/src/lib.rs:98-100`, `crates/tools/kraai-tool-list-files/src/lib.rs:115-119`
- Issue: both the top-level `path` and every entry `path` are absolute/display paths. That leaks host layout into the model context and repeats the directory prefix for every entry.
- Impact: unnecessary token use and avoidable exposure of local machine structure. It also makes outputs less portable and harder to compare in tests/snapshots.
- Suggested fix: return a normalized display root plus entry names, or include `relative_path` from the workspace root when possible. If absolute paths are still needed for outside-workspace calls, add both `path` and `workspace_relative_path: Option<String>` or a `base` field plus compact entry names.
- Suggested tests: assert workspace-relative output for a nested workspace directory and explicit absolute output only for outside-workspace listings.

### Medium: unreadable one-off entries abort the whole listing

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:110-121`
- Issue: a single `entry.metadata()?` failure aborts the entire directory listing. This can happen with permission errors, broken symlinks, races where a file is deleted between `read_dir` and `metadata`, or platform-specific special files.
- Impact: flaky behavior on active directories and poor usefulness on partially readable trees. Agents usually benefit from a partial listing plus per-entry errors.
- Suggested fix: return successful entries plus an `errors: Vec<ListFilesEntryError>` field. For entries where `metadata` fails, include the name/path and error string, or include the entry with `is_dir: null` and an `error` field. Use `symlink_metadata` if the intent is to describe the link itself instead of following the target.
- Suggested tests: include a broken symlink test on Unix and, where feasible, a permission-denied entry test. Assert the listing succeeds with a recorded entry error.

### Medium: no explicit symlink representation

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:39-44`, `crates/tools/kraai-tool-list-files/src/lib.rs:114-119`
- Issue: the output only exposes `is_dir`. Because `entry.metadata()` follows symlinks, a symlink to a directory is indistinguishable from a real directory, and a symlink to a file is indistinguishable from a real file.
- Impact: agents may make incorrect follow-up decisions and the safety model becomes harder to reason about around symlink escapes.
- Suggested fix: use `file_type()` or `symlink_metadata()` and return a richer type, for example `kind: "file" | "directory" | "symlink" | "other"` plus `target_kind` if following symlinks is desired.
- Suggested tests: Unix symlink-to-file and symlink-to-directory cases.

### Low: sort order is byte/name-only and can be less useful than directory-aware ordering

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:123`, `crates/tools/kraai-tool-list-files/src/lib.rs:175-207`
- Issue: sorting is only `a.name.cmp(&b.name)`. This is deterministic, which is good, but it mixes files and directories and uses platform/string conversion behavior after `to_string_lossy`.
- Impact: mostly usability. For agent workflows, directories-first often makes navigation cheaper and more predictable.
- Suggested fix: sort by `(is_dir desc, lowercase/name or raw OsString-compatible ordering, name)` depending on desired cross-platform behavior. Be explicit in tests.
- Suggested tests: add mixed file/directory names and assert the intended order.

### Low: tests manually clean temp directories and leak on panic

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:154-169`, repeated cleanup calls such as `crates/tools/kraai-tool-list-files/src/lib.rs:206`
- Issue: tests create temp directories manually and call `cleanup_temp_dir` at the end. Any panic before cleanup leaves files behind. The same pattern appears in sibling crates, but this crate can fix it locally or a shared test helper can be extracted.
- Impact: dirty `/tmp`, harder local debugging, and possible flaky later runs if names collide or permissions change.
- Suggested fix: use `tempfile::TempDir` as a dev-dependency or add a small shared test utility crate/module with RAII cleanup. If avoiding a new dependency, wrap `PathBuf` in a local guard that removes on `Drop`.
- Suggested tests: no behavior test needed; this is test infrastructure.

### Low: manifest has likely test-only or macro-only dependencies mixed into normal dependencies

- References: `crates/tools/kraai-tool-list-files/Cargo.toml:9-18`
- Issue: `tokio` is only used by tests, and `serde_json` appears only in tests through `ToolOutput::Success` data indexing. `toon-format` may be required indirectly by the `toon_tool!` macro expansion, but that is not obvious from local code. Keeping everything in `[dependencies]` makes the library dependency surface harder to audit.
- Impact: minor compile graph and maintainability cost. It also makes unused dependency hygiene harder as the workspace grows.
- Suggested fix: move test-only dependencies to `[dev-dependencies]` where possible. If `toon-format` must remain for macro expansion, leave a short comment or rely on workspace/hakari conventions if that is the project standard.
- Suggested verification: run `cargo machete` or an equivalent dependency linter after moving dependencies, then `just check`.

### Low: tests miss outside-workspace execution behavior

- References: `crates/tools/kraai-tool-list-files/src/lib.rs:293-330`
- Issue: assessment tests cover outside paths, but `call` behavior for outside paths is not tested. Sibling `read_files` explicitly tests that parent traversal can read outside workspace after assessment (`crates/tools/kraai-tool-read-file/src/lib.rs:210-244`).
- Impact: expected policy is implicit. Future maintainers may accidentally block or allow outside listing without noticing.
- Suggested fix: add an execution test that lists a parent-traversal outside directory and asserts the behavior the project wants. If outside reads are allowed with approval, assert success. If the new safety direction is to re-check policy at execution time, assert blocking without approval.

## Refactor opportunities

- Extract a shared filesystem listing helper into `kraai-tool-core` or a small internal module if more tools need directory enumeration. It should own path resolution, symlink policy, bounded output, partial errors, and async blocking strategy.
- Consider a common test helper for `ToolContext` and temporary workspaces. The same `tool_config`, `tool_context`, `make_temp_dir`, and cleanup pattern is duplicated across several tool crates.
- Consider aligning `list_files` with `search_files` output conventions: `truncated`, count fields, and explicit skipped/error reporting. Search already has a `MAX_MATCHES` guard (`crates/tools/kraai-tool-search-files/src/lib.rs:17`, `crates/tools/kraai-tool-search-files/src/lib.rs:128-134`); list should have the same kind of budget control.

## Suggested priority order

1. Add bounded output and truncation metadata.
2. Move listing work off async executor threads.
3. Decide and enforce symlink/outside-workspace policy at execution time.
4. Add partial-error handling for per-entry metadata failures.
5. Improve output shape and tests once the safety/performance behavior is explicit.
