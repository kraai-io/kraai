# Background Session Streaming and Approval Indicator

## Summary
Add automated coverage at the runtime and TUI layers to prove that session-local streaming/tool execution continues when the user creates or switches to another session. Surface a per-session approval-waiting indicator in both frontends using existing tool events, without changing persistence or introducing new cross-process state.

## Implementation Plan

### 1. Add a runtime test harness for concurrent session activity
Create a test-only harness in [crates/agent-runtime/src/lib.rs](/home/ominit/code/agent/crates/agent-runtime/src/lib.rs) that can:

- build a `RuntimeInner` with temp persistence instead of `persistence::init()`
- register a scripted mock provider with controllable streaming order
- register a mock tool whose assessment always requires approval
- capture emitted `Event`s in order for assertions

The mock provider should support this scripted flow:

1. Session A first reply streams in multiple chunks and pauses mid-stream.
2. Session B reply can start and complete while A is still paused.
3. Session A then finishes with a valid `<tool_call>...</tool_call>` block.
4. After tool execution, Session A continuation reply streams and completes.

The mock tool should:

- return `ExecutionPolicy::AlwaysAsk`
- provide deterministic `description` and `output`
- make it easy to assert that the tool actually executed for Session A only

### 2. Add runtime concurrency tests
Add runtime tests in [crates/agent-runtime/src/lib.rs](/home/ominit/code/agent/crates/agent-runtime/src/lib.rs) covering:

1. `background_session_stream_continues_after_switch`
   - Create Session A.
   - Send a message in A and let its stream start but not finish.
   - Create/load Session B while A is still streaming.
   - Send a message in B.
   - Assert both sessions emit their own `StreamStart`/`StreamChunk`/`StreamComplete` events.
   - Assert Session A completes even though B became the active UI session.

2. `background_session_tool_approval_and_continuation_work_after_switch`
   - Let Session A produce an approval-required tool call.
   - Load Session B before approving A’s tool.
   - Call `approve_tool(session_a, call_id)` and `execute_approved_tools(session_a)` while B is current.
   - Assert `ToolResultReady`, `HistoryUpdated`, continuation `StreamStart`, and continuation `StreamComplete` all occur for A.
   - Assert Session B history/tip remains independent.

Runtime acceptance criteria:

- Switching sessions never cancels another session’s stream.
- Only one active stream per session remains enforced.
- Tool approval/execution is keyed by `session_id`, not current UI selection.
- Continuation after tool execution resumes on the original session.

### 3. Add TUI session-level approval indicator
Update [crates/tui/src/app.rs](/home/ominit/code/agent/crates/tui/src/app.rs) and [crates/tui/src/app/ui.rs](/home/ominit/code/agent/crates/tui/src/app/ui.rs) so the sessions menu derives a waiting state from existing `pending_tools`.

Rule:

- a session is “waiting for approval” iff it has at least one `PendingTool` with `approved == None`

Render change in the sessions menu:

- append a compact session badge/suffix for waiting sessions
- keep the current-session marker intact
- do not mark sessions whose tools are already approved or denied

Default rendering choice:

- TUI suffix text: ` [approval]`
- example: `Testing plan (current) [approval]`

This is intentionally client-local and derived from already-received events; no runtime/session DTO changes are needed.

### 4. Add desktop session-level approval indicator
Update [apps/agent-desktop/src/App.tsx](/home/ominit/code/agent/apps/agent-desktop/src/App.tsx) so the session sidebar shows a visible badge for sessions waiting on user approval.

Rule matches TUI:

- show the badge when that session has at least one `pendingTools` entry with `approved === null`

UI behavior:

- badge remains visible even when the delete button is hidden
- switching into that session continues to show the existing permission dialog behavior
- approved-but-not-yet-executed tools do not show the “waiting for approval” badge

Default label:

- desktop badge text: `Approval`

### 5. Add TUI tests for session switching and indicator rendering
Extend tests in [crates/tui/src/app.rs](/home/ominit/code/agent/crates/tui/src/app.rs) with:

1. a behavior test proving a background session tool event is retained after switching to another session
   - receive `ToolCallDetected` for Session A
   - load/switch to Session B
   - assert Session A’s pending tool still exists in `state.pending_tools`

2. a sessions-menu rendering/snapshot test showing the approval indicator on the correct session row

3. a small guard test that approved/denied tools do not count as “waiting for approval”

## Public APIs / Interfaces / Types
No public Rust or TypeScript API changes are required.

The approval indicator will be derived client-side from existing runtime events:

- `ToolCallDetected`
- `ToolResultReady`

No new persisted session fields and no new runtime event variants are planned.

## Test Cases and Scenarios

### Runtime
- Session A stream remains active while Session B is created or loaded.
- Session B can stream while Session A is still active.
- Session A emits an approval-required tool call after Session B becomes current.
- Approving/executing Session A’s tool while Session B is current succeeds.
- Session A continuation stream resumes after tool execution.
- Session histories and tips remain isolated.

### TUI
- Background session pending tool state survives session switches.
- Sessions menu shows ` [approval]` only on sessions awaiting approval.
- Current session labeling and approval labeling render together correctly.

### Desktop
- Session sidebar shows `Approval` badge for the correct session.
- Badge disappears when `ToolResultReady` removes the pending tool.
- No new desktop test runner will be introduced in this pass.

## Verification
After implementation, run:

1. `cargo test -p agent-runtime`
2. `cargo test -p tui`
3. `just lint`
4. `cargo clippy --all-targets -- -D warnings`
5. `just typecheck-desktop`

## Assumptions and Defaults
- “Waiting for approval” means at least one unhandled tool request for that session, not merely approved-awaiting-execution state.
- The requested indicator belongs in the session lists for both frontends, not in persistence metadata.
- Background-session behavior will be validated primarily through runtime event ordering and per-session history assertions.
- No `agent-ts-bindings` rebuild is needed because this plan does not change exported bindings.
