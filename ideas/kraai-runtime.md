# kraai-runtime Findings

Scope: `crates/kraai-runtime` implementation and runtime tests.

## High Severity

### Deadlock on tool-call parse errors

- Location: `crates/kraai-runtime/src/runtime/tool_calls.rs:286-307`
- Impact: If `AgentManager::parse_tool_calls_from_content` returns `Err`, the runtime still holds `self.agent_manager.lock().await` from line 287 and then tries to acquire the same mutex again at lines 294-297. `tokio::sync::Mutex` is not reentrant, so this path deadlocks the spawned stream task. The session remains active, queued messages do not drain, and client calls requiring the agent lock can hang.
- Why tests miss it: `parse_failure_history_write_error_stops_continuation_and_recovers` covers a parse-failure history write error, but that flows through the `Ok((tool_calls, failed))` branch with non-empty `failed`, not the `Err(error)` branch.
- Suggested fix: Do not perform recovery inside the scope that owns the first agent lock. Return an enum/result from the locked block, drop the guard, then clear active turn, emit events, and drain the queue. Add a test with an `AgentManager` dependency or store/tool setup that forces `parse_tool_calls_from_content` itself to return `Err`.

## Medium Severity

### Fire-and-forget handle methods hide runtime command failures

- Location: `crates/kraai-runtime/src/handle.rs:213-243`, `306-312`, `371-390`, `405-418`; dispatch handling at `crates/kraai-runtime/src/runtime/dispatch.rs:91-100`, `146-155`, `278-311`
- Impact: Public APIs like `send_message`, `delete_session`, `approve_tool`, `deny_tool`, `continue_session`, and `execute_approved_tools` only report whether the command was enqueued. Actual runtime failures are converted to `Event::Error` or silently ignored, so callers cannot reliably know whether the operation succeeded. This makes CLI/TUI flows race-prone and hard to test.
- Suggested fix: Give commands that can fail a oneshot response and return the runtime result to the caller. Keep events for observability, but do not make them the only error channel. At minimum, `send_message`, `delete_session`, `approve_tool`, and `deny_tool` should report invalid session/call-id failures directly.

### Starting a new stream aborts the previous task without recovering agent state

- Location: `crates/kraai-runtime/src/runtime/streaming.rs:153-163`
- Impact: `start_stream_job` overwrites `active_streams[session_id]` and aborts the previous task, but it does not cancel or abort the previous streaming message in `AgentManager`. Existing callers try to prevent overlap through `is_turn_active`, but this helper is a sharp edge: any missed guard, race, or future call site can leave a streaming assistant message and active turn stuck in persistence.
- Suggested fix: Move same-session replacement into an explicit cancel/recover path, or make `start_stream_job` reject when a session already has an active stream. Add a debug assertion or returned error so overlap is caught at the boundary.

### Config watcher can reload too often and block a runtime worker

- Location: `crates/kraai-runtime/src/runtime/config.rs:37-101`
- Impact: A `std::sync::mpsc::Receiver` is consumed directly inside a `tokio::spawn` task at lines 59 and 82. That blocks a Tokio worker thread. Also, every write event sends `Command::LoadConfig` with no debounce or coalescing; `save_settings_document` already calls `load_providers_config` and emits `ConfigLoaded` at lines 127-137, so saving settings can produce duplicate reloads/events.
- Suggested fix: Use `tokio::task::spawn_blocking` for the blocking notify receiver, forward through a Tokio channel, and debounce/coalesce file changes. Suppress watcher-triggered reloads caused by the runtime's own atomic save, or tolerate them with versioning so clients do not see duplicate `ConfigLoaded` bursts.

### Provider/settings validation errors can be swallowed

- Location: `crates/kraai-runtime/src/settings.rs:219-222`, `252-255`
- Impact: `validate_provider_config` and `validate_model_config` errors are converted to an empty validation list with `unwrap_or_default()`. If the registry fails validation for an internal/config-shape reason, settings can be accepted even though provider construction later fails.
- Suggested fix: Preserve validation call failures as `SettingsValidationError` entries. If provider validation returns `Result<Vec<_>>`, an `Err` should fail the settings document with the provider/model field prefix and the original message.

### Tool execution is serial even for independent tool calls

- Location: `crates/kraai-runtime/src/runtime/tool_calls.rs:10-51`
- Impact: `execute_tool_requests` awaits each approved tool one at a time. A batch of slow independent read-only tools will take the sum of their latencies, delaying continuation and queued messages. This conflicts with the runtime's responsiveness goals under load.
- Suggested fix: Add a concurrency policy to tool execution. Run independent tools concurrently with a bounded limit, while preserving deterministic result ordering by collecting `(index, result)` and sorting before persistence/events. Keep sequential mode for tools that mutate shared state or require ordering.

## Low Severity / Maintainability

### RuntimeCore is accumulating orchestration responsibilities

- Location: `crates/kraai-runtime/src/runtime/core.rs:19-29`, with large impls spread across `dispatch.rs`, `streaming.rs`, `tool_calls.rs`, and `config.rs`
- Impact: The module split helps, but one shared `RuntimeCore` owns command dispatch, config watching, auth forwarding, stream lifecycle, queueing, tool execution, and event emission. This makes lock ordering and recovery paths hard to reason about, as shown by the parse-error deadlock.
- Suggested fix: Extract explicit coordinators with narrow state ownership: `StreamSupervisor`, `MessageQueue`, `ToolExecutionSupervisor`, and `ConfigReloader`. Keep `RuntimeCore` as composition plus command routing.

### RuntimeBuilder cannot be cleanly shut down

- Location: `crates/kraai-runtime/src/runtime/builder.rs:46-80`
- Impact: `build` spawns an unnamed OS thread and returns only `RuntimeHandle`. Dropping handles eventually closes the command channel, but there is no join handle, shutdown method, or way to observe background runtime termination. Tests use a separate harness with abortable Tokio tasks instead of the real builder.
- Suggested fix: Return a runtime owner/guard or add `RuntimeHandle::shutdown` that closes the command channel and joins the background thread. Add an integration test that exercises `RuntimeBuilder::build`, not only the custom harness.

### Broadcast event drops are ignored everywhere

- Location: `crates/kraai-runtime/src/runtime/core.rs:15-17`; channel capacity at `crates/kraai-runtime/src/runtime/builder.rs:48`
- Impact: `emit_event` discards send errors and the broadcast channel has fixed capacity 1024. Under heavy streaming, slow clients receive `Lagged` errors and miss chunks/history notifications; with no sequence numbers or snapshots in events, clients cannot reliably repair UI state.
- Suggested fix: Add event sequence numbers or require clients to resync from authoritative state after lag. Consider logging lag/drop metrics and separating high-volume stream chunks from control/state events.

### JSON serialization failures are converted to empty strings

- Location: `crates/kraai-runtime/src/runtime/dispatch.rs:266`, `crates/kraai-runtime/src/runtime/tool_calls.rs:212`, `354-360`
- Impact: `serde_json::to_string(...).unwrap_or_default()` hides serialization failures and can surface empty args/output in the UI. `serde_json::Value` should generally serialize, so failures are rare, but silently replacing data with `""` makes diagnostics worse.
- Suggested fix: Use `unwrap_or_else(|error| format!(r#"{{"error":"failed to serialize: {error}"}}"#))` or propagate a structured error event. Prefer a typed API field over pre-serialized JSON strings where possible.

### Test harness duplicates setup and skips tests on CA issues

- Location: `crates/kraai-runtime/src/runtime/tests/harness.rs:729-1009`, CA skip at `837-841`
- Impact: The harness repeats temp-dir, workspace, profile, store, and tool setup across constructors. Several tests return `Ok(())` when `OpenAiCodexAuthController::new()` fails due to missing system CA certs, which can hide runtime regressions on minimal CI images.
- Suggested fix: Extract a reusable fixture builder and inject a fake auth controller or auth-status adapter so runtime tests do not depend on system CA availability unless they are specifically testing OpenAI auth initialization.

## Test Gaps To Add

- A regression test for the `process_completed_stream_output` `Err(error)` branch that proves the runtime emits an error and remains responsive.
- A `RuntimeBuilder::build` lifecycle test covering startup, config load, handle drop/shutdown, and background thread termination.
- Config watcher tests for atomic save, duplicate reload suppression/debounce, and non-target files in the same directory.
- Backpressure tests for event lag and command queue saturation, especially during high-volume streams.
- Tests for direct API error responses after converting fire-and-forget commands to oneshot-returning commands.
