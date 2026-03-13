# Runtime Checkpointing And Restart Recovery

## Why this matters

The repo already persists sessions and messages, but volatile runtime state still lives in memory. That means a process restart can keep chat history while silently losing pending approvals, active turn metadata, pending workspace changes, and model/provider selection for the current turn.

## Current gap

`SessionRuntimeState` in `crates/agent/src/lib.rs` keeps `pending_tool_calls`, `pending_tool_config`, `last_model`, `last_provider`, and `active_turn_profile` in memory only. `prepare_session()` can rehydrate basic session state, but there is no persisted representation of runtime turn state or recovery logic for interrupted streams.

## Goal

Persist enough runtime state to resume or safely reconcile a session after restart, crash, or process replacement.

## Plan

1. Introduce a serializable `SessionRuntimeSnapshot` in `types` or a small new persistence-facing module.
2. Persist the snapshot whenever turn state changes:
   - stream starts
   - tool calls are detected
   - tool approvals change
   - pending workspace config changes
   - stream completes, fails, or is cancelled
3. Extend the persistence layer with snapshot load/save/delete primitives keyed by session id.
4. Update `AgentManager::prepare_session()` to hydrate runtime state from the snapshot before rebuilding derived session flags.
5. Define recovery rules for stale states:
   - pending stream placeholder exists but no live task exists
   - pending tool approvals exist after restart
   - pending workspace switch was queued but never promoted
6. Add an explicit recovered state marker so clients can distinguish fresh state from recovered state if needed later.

## Milestones

1. Persist snapshots without changing behavior yet.
2. Hydrate snapshots on session preparation.
3. Add recovery rules for interrupted streams and pending approvals.
4. Add cleanup for snapshots when sessions are deleted.

## Validation

1. Add tests that restart the agent manager after:
   - tool detection but before approval
   - approval but before execution
   - stream placeholder creation but before first chunk
   - mid-stream cancellation
2. Verify no session becomes permanently stuck in a streaming or locked state.
3. Verify recovered pending approvals still point at valid call ids and args.

## Risks

The main risk is persisting too much ephemeral detail and locking in a bad state shape. Keep the first version intentionally small and based on semantic state, not task handles or transport internals.
