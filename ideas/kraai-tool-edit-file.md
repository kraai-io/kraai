# kraai-tool-edit-file Findings

Scope: `crates/tools/kraai-tool-edit-file`.

## High

1. **Successful edits do not refresh or invalidate the file-read snapshot**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:125-138`, `207-218`; compare `kraai-tool-read-file/src/lib.rs:77-92` and `kraai-tool-open-file/src/lib.rs:70-83`.
   - Impact: after a successful write, `edit_file` returns `ToolCallResult::success(...)` with no `file_reads` delta. The session history still contains the old read hash until some later read/open refresh occurs. A later turn against the same file can fail with "file changed since it was last read" even though the change was made by Kraai itself. This creates unnecessary retry loops and makes tool state less truthful.
   - Suggested fix: after writing, compute the sha256 for the newly written contents and return `success_with_deltas(..., vec![file_read_refresh_delta(path, new_sha)])`. For `create=true`, consider emitting the same delta as a read-refresh if created contents are now known exactly, or add a deliberate "write invalidates read" operation in the shared file state model.

2. **Check-then-write create mode has a race and can overwrite a just-created file**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:182-204`.
   - Impact: `create_file` checks `path.exists()` and then calls `fs::write`. Another process can create the file between those operations; `fs::write` will then truncate and overwrite it. For an agent edit tool, this is a reliability and data-loss risk under concurrent sessions or external file changes.
   - Suggested fix: use `OpenOptions::new().write(true).create_new(true).open(path)` and then write all bytes. Keep the existing parent validation if useful for clearer errors, but make the actual create atomic.

3. **Writes are non-atomic and can leave truncated/corrupt files on failure**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:216-217` and `203-204`.
   - Impact: `fs::write` truncates the target before writing. Disk-full, permission, crash, or interruption scenarios can leave a partially written file. This conflicts with the repo priority of predictable behavior during failures.
   - Suggested fix: add a shared atomic text-write helper in `kraai-tool-core`: write to a temp file in the same directory, flush/sync as appropriate, then `rename` over the destination for edit mode. For create mode, combine this with create-new semantics or write the final path via `create_new`.

## Medium

4. **Sync filesystem IO is executed directly inside async tool calls**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:125-138`, `182-218`; shared helpers in `kraai-tool-core/src/lib.rs:398-420`.
   - Impact: reading, hashing, and writing whole files are blocking operations on the async runtime worker. Large files or slow filesystems can stall unrelated runtime work. Other tools currently share this smell, but write paths are the highest risk because they may block longer and hold the runtime through failure handling.
   - Suggested fix: either make the tool trait explicitly blocking and execute tools on a blocking pool, or move filesystem-heavy calls into `tokio::task::spawn_blocking`. Longer term, centralize file IO policy in `kraai-tool-core` so read/write/search tools behave consistently.

5. **Line ending behavior is under-specified and untested**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:28-31`, `238-269`, `393-423`.
   - Impact: `index_lines` strips `\r` from logical line content, so `old_text` for CRLF files must use `\n`, while replacement ranges preserve untouched CRLF separators outside the replaced span. Multi-line replacements can easily produce mixed line endings if `new_text` contains `\n`. That may be acceptable, but the contract is not documented or tested.
   - Suggested fix: decide on policy. Either preserve the existing file's dominant newline style by normalizing `new_text` before replacement, or document that `new_text` is inserted byte-for-byte and add tests for CRLF single-line and multi-line edits.

6. **Appending to an existing non-empty file is not expressible**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:315-350`, `393-423`.
   - Impact: a trailing empty logical line is not indexed for files ending in `\n`, so line `N+1` cannot be targeted as an insertion point. Empty files get a special case (`1-1` with empty `old_text`), but non-empty files cannot append without replacing the last line and manually including its old contents. This is awkward for LLM use and encourages larger replacements than necessary.
   - Suggested fix: add explicit insertion operations, e.g. `{ line, text, position: before|after }`, or allow a zero-width range such as `start_line = end_line + 1` / `old_text = ""` with clear semantics. Cover appending before/after final newline in tests.

7. **The implementation and tests are a single 1,080-line file**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:15-423` for implementation and `425-1080` for tests.
   - Impact: this is already a context-heavy file for a core editing primitive. It mixes schema, assessment, path/state guards, edit application, line indexing, and extensive integration-style tests. That makes future changes more expensive and increases the odds of localized fixes duplicating logic.
   - Suggested fix: split into focused modules such as `args.rs`, `apply.rs`, `io.rs`, and `tests/` or private test modules. The pure edit engine (`index_lines`, `validate_edit`, `apply_edits`) is a good first extraction because it can be tested without tool/runtime setup.

## Low

8. **Tests use hand-rolled temp directories instead of automatic cleanup**
   - References: `crates/tools/kraai-tool-edit-file/src/lib.rs:470-485` and repeated `cleanup_temp_dir` calls throughout tests.
   - Impact: panics before cleanup leak temp directories. The helper is duplicated across tool crates, which makes test infrastructure noisier than the behavior under test.
   - Suggested fix: use `tempfile::TempDir` from a workspace dev-dependency or add a small shared test helper crate/module. This also removes manual cleanup calls from every test.

9. **Missing tests for important edge cases**
   - References: current tests cover normal edits, blank lines, empty files, overlap, invalid args, create failures, and prior-read guards in `crates/tools/kraai-tool-edit-file/src/lib.rs:539-1079`.
   - Gaps:
     - CRLF files and mixed newline replacement behavior.
     - Unicode edits where byte offsets must remain on UTF-8 boundaries.
     - Paths that are symlinks into/out of the workspace for both assessment and actual writes.
     - Directory target for edit mode (`read_text_path` handles it indirectly, but the tool-level error contract is not asserted).
     - Post-edit tool-state delta expectations, if fixed per finding 1.
   - Suggested fix: add pure `apply_edits` tests for newline/unicode behavior and tool-level tests for symlink/path/state behavior.

10. **`create` and `contents`/`edits` make the API more error-prone than separate modes**
    - References: schema at `crates/tools/kraai-tool-edit-file/src/lib.rs:34-48`, validation at `154-180`.
    - Impact: the current shape permits many invalid combinations that are only rejected at runtime (`create=true` plus edits, `create=false` plus contents, missing edits, empty edits). This burns model/tool attempts and adds validation logic.
    - Suggested fix: model arguments as an enum/tagged mode if the toon schema can express it, or split into `create_file` and `edit_file` tools. If the single tool remains, include examples for invalid/valid mode boundaries in tests and tighten schema descriptions.
