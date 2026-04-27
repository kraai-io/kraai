# kraai-agent Findings

Scope: `crates/kraai-agent` at the current checkout. I did not modify source files.

## High

### Workspace changes can take effect while a turn is active

- References: `crates/kraai-agent/src/manager/sessions.rs:123`, `crates/kraai-agent/src/manager/sessions.rs:128`, `crates/kraai-agent/src/manager/sessions.rs:133`, `crates/kraai-agent/src/manager/streaming.rs:25`, `crates/kraai-agent/src/manager/streaming.rs:32`, `crates/kraai-agent/src/manager/tool_calls.rs:71`
- Impact: `set_workspace_dir` immediately persists `session.workspace_dir`, then stores the new value as `pending_tool_config`. A running turn keeps using `active_tool_config`, but later calls that resolve profiles or workspace metadata read `session.workspace_dir`. This can mix old-turn tool execution with new-workspace profile resolution and can make active-turn behavior depend on when the UI changes workspace.
- Concrete example: `prepare_continuation_stream` resolves the selected profile from persisted `session.workspace_dir` before it reads the active runtime workspace (`streaming.rs:134-156`). Existing tests cover AGENTS.md staying on the active workspace (`prompts.rs:242`), but not workspace-specific profile overrides. A workspace switch during an active turn could make continuation validation fail with "Selected profile is unavailable" or pick a different override while tools still assess against the old workspace.
- Suggested fix: reject workspace changes while `is_turn_active(session_id)` or pending/in-flight tools exist, or keep requested workspace changes entirely in runtime state until the next `prepare_start_stream` promotes them. If UI needs to show a pending workspace, expose that from runtime state without persisting it as the session workspace yet.
- Suggested tests: active turn with workspace A selected profile overridden in A, call `set_workspace_dir` to workspace B where the profile is missing or different, then ensure continuation still uses A consistently or the workspace change is rejected.

### Active turn state can remain locked after stream cancellation or abort

- References: `crates/kraai-agent/src/manager/streaming.rs:384`, `crates/kraai-agent/src/manager/streaming.rs:404`, `crates/kraai-agent/src/manager/tool_calls.rs:232`, `crates/kraai-runtime/src/runtime/streaming.rs:548`
- Impact: `abort_streaming_message` and `cancel_streaming_message` remove/restore streaming state but do not clear `active_turn_profile`, `active_turn_auto_approve`, or `active_turn_tool_state_snapshot`. Runtime clears the turn in at least one cancel path, but the crate API itself allows callers/tests to abort or cancel and leave the session locked. After that, `prepare_start_stream` rejects new user messages as "current turn is active", and profile changes are blocked.
- Suggested fix: make terminal stream operations return enough typed state for the caller to clear reliably, or move active-turn cleanup into `AgentManager` methods that represent terminal outcomes. At minimum, document that every abort/cancel caller must invoke `clear_active_turn` and add assertions in tests.
- Suggested tests: start a stream, call `abort_streaming_message` directly, then assert `is_turn_active` is false or that a new start is accepted. Do the same for `cancel_streaming_message` with both empty and non-empty content.

## Medium

### Ready tool executions are not emitted in queue order

- References: `crates/kraai-agent/src/manager/tool_calls.rs:263`, `crates/kraai-agent/src/manager/tool_calls.rs:268`, `crates/kraai-agent/src/manager/tool_calls.rs:276`
- Impact: pending tools are stored in a `HashMap`, and `take_ready_tool_executions` collects ready IDs from map iteration order. `list_pending_tools` sorts by `queue_order`, but execution does not. Multiple approved tools from one assistant message can execute in nondeterministic order, which is especially risky when tool outputs mutate tool state or files.
- Suggested fix: sort `ready_ids` by `PendingToolCall.queue_order`, or store pending calls in an ordered structure keyed by queue order. Preserve stable execution order across approved, denied, and auto-approved calls.
- Suggested tests: parse two tool calls, approve both, call `take_ready_tool_executions`, and assert execution order matches detected `queue_order`.

### Tool-state refresh ignores workspace normalization and reads stored absolute paths directly

- References: `crates/kraai-agent/src/tool_state.rs:47`, `crates/kraai-agent/src/tool_state.rs:49`, `crates/kraai-agent/src/tool_state.rs:61`, `crates/kraai-agent/src/tool_state.rs:62`, `crates/tools/kraai-tool-open-file/src/lib.rs:70`
- Impact: `refresh_and_render_system_prompt` takes `workspace_dir` but ignores it, then reads each opened path with `read_text_path(Path::new(&path))`. Today `open_file` stores resolved display paths, so the common path works, but this duplicates path semantics outside the tool layer and trusts whatever path string is in persisted deltas. If state was created by older code, a custom tool, or corrupted persistence, the system prompt can pin arbitrary absolute paths without the same workspace assessment path used by tools.
- Suggested fix: store opened files as canonical tool paths with an explicit workspace binding, or refresh through a shared helper that validates/resolves paths against the active workspace. Remove the unused `_workspace_dir` or make it part of the check.
- Suggested tests: a snapshot containing `opened_files` outside the active workspace should either be excluded, marked as unavailable, or require an explicit policy decision.

### Failed tool-result persistence leaves in-flight counts uncleared

- References: `crates/kraai-agent/src/manager/tool_calls.rs:263`, `crates/kraai-agent/src/manager/tool_calls.rs:280`, `crates/kraai-runtime/src/runtime/tool_calls.rs:227`, `crates/kraai-runtime/src/runtime/tool_calls.rs:252`
- Impact: `take_ready_tool_executions` removes pending calls and increments `in_flight_tool_calls`. If `add_tool_results_to_history` fails, runtime clears the active turn and returns before `finish_tool_executions`. The stale in-flight count remains in runtime state, so `has_unfinished_tools_for_message` can stay true and suppress continuations for that source message.
- Suggested fix: ensure in-flight tracking is cleaned up in a `finally`-style path after tool execution attempts, or provide an `abort_tool_executions` method that decrements counts when result persistence fails.
- Suggested tests: inject a `MessageStore` save failure during `add_tool_results_to_history`, then assert `has_unfinished_tools_for_message` is false after recovery.

### `complete_message` persistence recovery can race with duplicate completion calls

- References: `crates/kraai-agent/src/manager/streaming.rs:366`, `crates/kraai-agent/src/manager/streaming.rs:367`, `crates/kraai-agent/src/manager/streaming.rs:373`, `crates/kraai-agent/src/manager/streaming.rs:374`
- Impact: `complete_message` removes the streaming state before saving the completed message, then reinserts only if save fails. A second terminal call for the same message during the save window gets `Ok(None)` and may skip cleanup/events even though the original completion has not committed. Runtime likely serializes most paths, but the method itself is public and async.
- Suggested fix: keep a per-message terminal-state guard, or hold state until save succeeds and mark it terminal. If concurrent callers are unsupported, document it and add tests around duplicate terminal calls.

## Low / Maintainability

### `AgentManager` mixes session persistence, stream staging, prompt assembly, tool approval, and runtime state

- References: `crates/kraai-agent/src/manager/mod.rs:138`, `crates/kraai-agent/src/manager/sessions.rs:23`, `crates/kraai-agent/src/manager/streaming.rs:15`, `crates/kraai-agent/src/manager/tool_calls.rs:4`, `crates/kraai-agent/src/manager/prompts.rs:38`
- Impact: the split into files helps, but all concerns still mutate shared `SessionRuntimeState` through one type. This makes lifecycle invariants implicit: active turn, pending workspace, stream state, pending tools, and in-flight tools must be cleared/promoted in the right order by external runtime code.
- Suggested fix: extract a `TurnState`/`TurnCoordinator` with explicit transitions (`start_user_turn`, `stream_completed`, `tools_detected`, `tools_finished`, `turn_failed`, `turn_cancelled`). Make invalid transitions return errors. Keep persistence methods separate from lifecycle state.

### Profile loading rejects an entire layer on one bad profile

- References: `crates/kraai-agent/src/profiles/mod.rs:128`, `crates/kraai-agent/src/profiles/mod.rs:151`, `crates/kraai-agent/src/profiles/mod.rs:160`, `crates/kraai-agent/src/profiles/mod.rs:181`
- Impact: one invalid profile in `.kraai/agents.toml` drops all valid profiles from that file. That is simple, but it is brittle for a user-editable config and makes unrelated profile edits affect selected profiles.
- Suggested fix: parse the file once, then validate profiles independently. Return valid profiles plus per-profile warnings where possible. Keep whole-file failure only for TOML syntax errors or duplicate IDs if duplicate handling cannot be deterministic.
- Suggested tests: file with one valid and one invalid profile should retain the valid profile and report a warning for the invalid one.

### Blocking filesystem reads run inside async request preparation

- References: `crates/kraai-agent/src/manager/prompts.rs:23`, `crates/kraai-agent/src/profiles/mod.rs:137`, `crates/kraai-agent/src/tool_state.rs:62`, `crates/tools/kraai-tool-core/src/lib.rs:411`
- Impact: preparing a stream performs synchronous reads of `AGENTS.md`, profile TOML, and every opened file. Large pinned files, slow filesystems, or network-mounted workspaces can block the async runtime worker thread and delay unrelated sessions.
- Suggested fix: use `tokio::fs`/`spawn_blocking` for these reads, add size caps for pinned file prompt injection, and consider caching profile resolution by workspace plus mtime.
- Suggested tests: unit tests for max pinned-file size/truncation once limits exist; integration test ensuring very large opened files do not build unbounded prompts.

### Test coverage misses tool parsing and execution lifecycle paths

- References: `crates/kraai-agent/src/manager/tests/mod.rs:1`, `crates/kraai-agent/src/manager/tool_calls.rs:4`, `crates/kraai-agent/src/manager/tool_calls.rs:263`
- Impact: existing manager tests cover sessions, streams, and prompts, but there is no dedicated `tool_calls` test module. The highest-risk behavior in this crate is tool-call parsing, approval, queueing, in-flight tracking, denial handling, and parse-failure history insertion.
- Suggested fix: add `manager/tests/tool_calls.rs` with focused tests for allowed/disallowed tools, parse failures, queue ordering, auto-approval vs pending approval, deny result payloads, and in-flight cleanup after success/failure.

### Minor dependency/code hygiene

- References: `crates/kraai-agent/Cargo.toml:11`, `crates/kraai-agent/Cargo.toml:13`
- Impact: `chrono` and `directories` appear unused in `kraai-agent`. Unused dependencies increase build surface and can confuse ownership of time/path behavior.
- Suggested fix: remove unused direct dependencies if `cargo machete` or `cargo tree -i` confirms they are not needed by feature unification.
