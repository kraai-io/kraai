# kraai-persistence review notes

Scope: `crates/kraai-persistence` as of this workspace state. The crate is currently a single implementation file, [crates/kraai-persistence/src/lib.rs](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:1), with direct use from agent/runtime session and streaming paths.

## Findings

### High: message writes are not crash-safe and can leave truncated/corrupt history

Evidence: `FileMessageStore::save` serializes and writes directly to the final message path with `fs::write` ([lib.rs:131](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:131)-[140](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:140)). In contrast, sessions use a temp file plus rename ([lib.rs:263](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:263)-[273](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:273)).

Impact: a process crash, power loss, ENOSPC, or interrupted write can leave `messages/<id>.json` partially written. Later `get` fails hard on JSON parse ([lib.rs:115](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:115)-[120](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:120)), which can break session history traversal, startup orphan cleanup, or deletion GC.

Suggested fix: introduce a shared atomic JSON write helper for both messages and sessions: write unique temp file in the same directory, flush/sync the file, rename, then best-effort sync the parent directory on Unix. Add tests that simulate an existing valid message plus a failed replacement path and assert the previous valid content remains readable.

### High: session save is atomic by rename but not durable across power loss

Evidence: `persist_sessions` writes a temp file and renames it ([lib.rs:263](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:263)-[273](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:273)), but it never flushes/syncs the temp file or containing directory before returning success.

Impact: after a crash or abrupt shutdown, callers may have observed `save` success while the new `sessions.json` data or rename has not reached durable storage. That is a reliability mismatch for session metadata, especially because agent stream completion and rollback paths depend on session tips being correct.

Suggested fix: centralize durable atomic writes. Use `tokio::fs::OpenOptions` plus `AsyncWriteExt::write_all`/`flush`, then `File::sync_all`, `rename`, and parent directory sync where supported. If full durability is intentionally out of scope, document that the store is only best-effort and name the helper accordingly.

### High: corrupt `sessions.json` bricks persistence initialization

Evidence: `FileSessionStore::load` parses the entire `sessions.json` map with one `serde_json::from_str` call and returns an error on any parse failure ([lib.rs:238](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:238)-[248](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:248)). `init` calls `load` and returns the error ([lib.rs:451](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:451)-[459](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:459)).

Impact: one corrupt metadata file prevents the whole app from starting, with no quarantine/recovery path. This is likely after the non-durable write path above, manual edits, or schema evolution mistakes.

Suggested fix: on parse failure, rename the bad file to a timestamped/ULID backup such as `sessions.json.corrupt.<ulid>`, start with an empty session map, and surface a warning event/log. Longer term, store sessions as individual records or JSONL so one corrupt session does not take all sessions down.

### High: orphan cleanup at startup can delete in-progress streaming tips after restart

Evidence: streaming assistant messages are not saved when they start. `start_streaming_message` only inserts the streaming message into in-memory `streaming_messages` and advances the session tip ([streaming.rs:307](/home/ominit/code/kraai/crates/kraai-agent/src/manager/streaming.rs:307)-[331](/home/ominit/code/kraai/crates/kraai-agent/src/manager/streaming.rs:331)). On startup, `init` loads sessions and immediately calls `cleanup_orphans` ([lib.rs:454](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:454)-[457](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:457)). Traversal stops when a tip message is missing ([lib.rs:283](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:283)-[289](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:289)), so ancestors behind that missing streaming tip are not marked referenced.

Impact: if the process crashes after the session tip advances to an unsaved streaming message, startup cleanup can classify the previous persisted conversation chain as orphaned and delete it. This is data loss.

Suggested fix: make tip updates and message persistence transactional at the logical level. Options: persist a placeholder streaming message before moving `tip_id`; on startup, detect `SessionMeta.tip_id` pointing at a missing message and roll the session back to a persisted `previous_tip`; or keep a session journal that records previous tip and pending tip. Add a restart test that advances `tip_id` to a missing message with an existing ancestor and verifies cleanup does not delete the ancestor.

### Medium: delete removes the session before GC succeeds, leaving partial failure states

Evidence: `FileSessionStore::delete` persists the session map without the deleted session ([lib.rs:383](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:383)-[390](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:390)), then calls `gc_orphaned_messages` ([lib.rs:392](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:392)-[395](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:395)). Existing test coverage asserts GC failures are surfaced ([lib.rs:698](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:698)-[725](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:725)).

Impact: callers receive an error, but the session is already gone from memory and disk. The user-visible operation may look failed while the session cannot be recovered normally. Retrying delete also loses the original tree, so leftover messages become generic startup-cleanup work rather than tied to that session.

Suggested fix: decide and encode semantics. Either treat GC as best-effort after a successful session delete and return success with logged cleanup errors, or use a tombstone/pending-delete state so failures can be retried with the original tree. Current behavior is the most confusing combination: destructive metadata change plus error.

### Medium: traversal has no cycle detection and can loop forever on corrupt data

Evidence: `collect_tree_messages` inserts each id into a `HashSet`, but it does not stop when an id has already been seen ([lib.rs:279](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:279)-[292](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:292)). Similar parent-chain walks exist in `AgentManager::cleanup_hot_cache_for_session` ([sessions.rs:60](/home/ominit/code/kraai/crates/kraai-agent/src/manager/sessions.rs:60)-[72](/home/ominit/code/kraai/crates/kraai-agent/src/manager/sessions.rs:72)) and `get_history_context` ([streaming.rs:449](/home/ominit/code/kraai/crates/kraai-agent/src/manager/streaming.rs:449)-[471](/home/ominit/code/kraai/crates/kraai-agent/src/manager/streaming.rs:471)).

Impact: a corrupt or malicious message file whose `parent_id` points to itself, or a two-message cycle, causes an infinite async loop in cleanup, deletion, list history, or startup orphan cleanup.

Suggested fix: add a shared message-chain traversal helper that tracks visited ids and returns a structured corruption error or truncates traversal with a warning. Use it in persistence and agent history/cache cleanup to avoid three separate implementations.

### Medium: message ids are used as filenames without validation

Evidence: `message_path` joins `format!("{}.json", id)` directly under `messages` ([lib.rs:85](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:85)-[87](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:87)). `MessageId::new` accepts any string ([kraai-types/lib.rs:235](/home/ominit/code/kraai/crates/kraai-types/src/lib.rs:235)-[238](/home/ominit/code/kraai/crates/kraai-types/src/lib.rs:238)).

Impact: production-created ids are ULIDs, but deserialized or test-created ids can contain `/`, `..`, path separators, or platform-problematic characters. That can write outside `messages`, make delete target arbitrary relative paths, or make `list_all_on_disk` unable to round-trip ids containing dots/slashes ([lib.rs:191](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:191)-[202](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:202)).

Suggested fix: either make `MessageId` a validated ULID/newtype for persisted messages, or encode ids for filenames with a reversible safe encoding. Add negative tests for `../x`, nested ids, and ids containing `.json`.

### Medium: hot cache is unbounded and can grow to all messages on disk

Evidence: every cold `get` inserts the message into `hot` ([lib.rs:122](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:122)-[126](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:126)). There is manual unload support ([lib.rs:151](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:151)-[154](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:154)), and agent session preparation unloads messages not in the selected session ([sessions.rs:60](/home/ominit/code/kraai/crates/kraai-agent/src/manager/sessions.rs:60)-[80](/home/ominit/code/kraai/crates/kraai-agent/src/manager/sessions.rs:80)), but other operations such as listing user input history traverse many sessions and call `get_history_context` ([sessions.rs:87](/home/ominit/code/kraai/crates/kraai-agent/src/manager/sessions.rs:87)-[114](/home/ominit/code/kraai/crates/kraai-agent/src/manager/sessions.rs:114)).

Impact: history search, cleanup, or GC can load a large portion of disk history into memory and keep it there until a session preparation happens. Long-running TUI sessions can accumulate unnecessary memory.

Suggested fix: replace the raw `HashMap` with a bounded LRU/weighted cache, or split traversal reads into cacheable and non-cacheable modes. At minimum, add metrics/logging for hot cache size and tests that `list_user_input_history` or cleanup does not permanently pin unrelated histories.

### Medium: synchronous `Path::exists` is used inside async paths and hides TOCTOU races

Evidence: async methods call `path.exists()` before async I/O in `get`, `delete`, `exists`, `list_all_on_disk`, and `load` ([lib.rs:111](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:111), [lib.rs:165](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:165), [lib.rs:176](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:176), [lib.rs:187](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:187), [lib.rs:234](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:234)).

Impact: these are small blocking filesystem stats on the async runtime. More importantly, the pre-checks are race-prone: a file can disappear after `exists` but before `read_to_string`/`remove_file`, turning benign missing data into an error.

Suggested fix: prefer async operations and handle `ErrorKind::NotFound` at the operation boundary. Example: `fs::read_to_string` then map `NotFound` to `Ok(None)`, `fs::remove_file` then ignore `NotFound`.

### Low: `thiserror` dependency is unused

Evidence: `crates/kraai-persistence/Cargo.toml` declares `thiserror.workspace = true` ([Cargo.toml:17](/home/ominit/code/kraai/crates/kraai-persistence/Cargo.toml:17)), but the crate imports only `color_eyre` errors and has no `thiserror` usage in [lib.rs](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:3).

Impact: minor dependency noise and compile graph churn. It also suggests error taxonomy was planned but not implemented.

Suggested fix: remove `thiserror` from the crate, or introduce typed persistence errors if callers need to distinguish corruption, missing records, durable-write failures, and GC failures.

### Low: persistence, GC, paths, and tests are all in one file

Evidence: [lib.rs](/home/ominit/code/kraai/crates/kraai-persistence/src/lib.rs:1) contains public traits, session metadata, message store, session store, path helpers, initialization, GC, and tests. The file is already 700+ lines, and this is a core reliability crate.

Impact: the persistence layer is still small, but changes to one concern pull unrelated code into context. That conflicts with the repo goal of avoiding context-polluting god files as the crate grows.

Suggested fix: split into `message_store.rs`, `session_store.rs`, `paths.rs`, `atomic_write.rs`, and `tests/` or focused test modules. Keep public re-exports in `lib.rs`.

## Missing tests to add first

- Atomic message replacement: failed write must not corrupt an existing message.
- Startup recovery with `SessionMeta.tip_id` pointing at a missing message while ancestors still exist.
- Corrupt `sessions.json` handling and backup/quarantine behavior.
- Message parent cycle detection for self-cycle and two-node cycle.
- Filename safety for invalid ids or encoded ids.
- Concurrent `FileMessageStore::save/delete/get` on the same id, including `NotFound` races.
- Cache growth behavior for cross-session history listing or explicit max-cache eviction.
