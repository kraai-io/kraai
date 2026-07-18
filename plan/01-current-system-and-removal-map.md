# Current System And Removal Map

> Status: inventory draft. “Remove” means the responsibility disappears.
> “Replace” means the behavior remains but receives a Nushell-native owner.
> Conditional removals are called out where they depend on an open decision.

## Current End-To-End Path

Today, a model-facing tool call crosses all of these layers:

1. `kraai-agent` adds a TOON execution preamble and concatenated tool schemas to
   the system prompt.
2. `kraai-runtime` watches the streamed assistant text for `<tool_call>` and
   truncates or defers visible content according to the current stream guard.
3. `kraai-tool-core::toon_parser` extracts each tag body, decodes TOON to JSON,
   and separates `tool` from the argument object.
4. `kraai-agent` checks the active profile, asks `ToolManager` to deserialize
   typed arguments, assesses risk, creates pending calls, and decides whether
   approval is required.
5. `kraai-runtime` schedules prepared tool calls, emits per-tool events,
   persists results, and decides whether to continue automatically.
6. Individual tool crates execute their operation and return a JSON value plus
   optional tool-state deltas.
7. `kraai-agent` formats each result as a tool-role message. Opened-file deltas
   are replayed into a snapshot and injected into subsequent system prompts.
8. `kraai-tui` decodes TOON again for display and maintains a pending-tool
   approval interface.

The Nushell redesign changes every stage except provider text streaming itself
and the underlying opened-file context concept.

## Crate Impact

### `kraai-types`

Replace:

- `ToolCall { call_id, tool_id, args: serde_json::Value }` with a script-oriented
  execution request containing raw Nushell source and execution identity.
- `ToolResult` with a script execution outcome capable of representing a final
  structured value, stdout/stderr, exit status, cancellation, timeout, sandbox
  denial, and state deltas.
- Tool-specific public names in profile summaries and runtime-facing DTOs with
  the selected command/capability vocabulary.

Preserve but likely rename or generalize:

- `CallId` as the parent script execution identity.
- `ToolStateSnapshot` and `ToolStateDelta`; they are valuable independently of
  TOON and typed tools.
- Sandbox configuration types, subject to the capability decision.

Delete and replace:

- Delete `RiskLevel`, `ExecutionPolicy`, and `ToolCallAssessment` rather than
  adapting them to scripts.
- Add an escalation-policy enum with `Deny`, `Prompt`, and `Allow` behavior.
- Add profile-owned sandbox permission sets describing the default filesystem,
  network, environment, and other isolation behavior once those dimensions are
  finalized.
- Add capability-based profile permission rules that decide allow/deny/prompt
  before the fallback escalation policy. They evaluate requested additions such
  as workspace write access rather than attempting to classify raw Nushell
  source.
- Keep execution isolation separate from policy. A script requests explicit
  capability additions; the profile's rules and fallback policy decide whether
  those additions are denied, allowed, or eligible for a one-shot prompt. The
  user decides whether that one complete script may run with prompted
  capabilities.
- Replace `SandboxPermissions` with script-oriented naming after the requested
  escalation shape is finalized.

### `kraai-tool-core`

Gut:

- TOON tag-body parsing and TOON-to-JSON decoding.
- `TypedTool`, erased adapters, `PreparedToolCall`, `ToolManager`, and schema
  concatenation.
- Tool preparation based on serde-deserializing a JSON argument object.
- Per-tool assessment and description hooks in their current form.

Retain by extraction or replacement:

- Filesystem path normalization, workspace-containment checks, line-number
  formatting, and safe file-opening helpers.
- `kraai-command-core` replaces this crate with the selected native embedded
  command contract and registry; there is no plugin/helper variant.
- The replacement contract must have one compile-time source of truth for the
  Nushell signature, concise argument help, examples, prompt rendering, and
  runtime registration.

The final architecture should not leave a misleading `kraai-tool-core` crate
whose main abstraction is no longer a tool.

### Individual `kraai-tool-*` crates

For `kraai-open-files`, `kraai-close-files`, and `kraai-edit-file`:

- Remove `toon_tool!` declarations, generated schema examples, JSON-facing DTO
  plumbing, `TypedTool` implementations, per-tool risk assessment, and
  descriptions used only by the approval UI.
- Preserve tested domain logic: containment rules, symlink behavior, edit
  validation, size limits, line numbering, search traversal, and opened-file
  state changes.
- Adapt the public surface to idiomatic Nushell positional parameters, flags,
  input/output signatures, help text, and structured values.
- Let Nushell parse and bind every invocation. The bridge receives native
  command-call data from Nushell and must not parse or rewrite model-authored
  source to distinguish positional arguments from pipeline input.
- Preserve compile-time generated schema/help and examples, but generate
  Nushell-native command documentation rather than TOON serialization schemas.
- Preserve one crate per Kraai command. Shared domain mechanics belong in narrow
  reusable crates, but command implementations are not grouped by convenience;
  this prevents future substantial commands such as subagent-backed web search
  from accumulating in a generic command crate.

Delete `kraai-tool-read-file`, `kraai-tool-list-files`, and
`kraai-tool-search-files` completely:

- Nushell `open`, `ls`, `glob`, pipelines, and normal external commands replace
  the read/list behavior.
- Package `rg` as an ordinary executable on Kraai's runtime PATH. Do not create a
  Kraai wrapper or embedded search command unless later evaluation proves a
  specific deficiency that cannot be addressed through normal Nushell usage.
- Extract only genuinely shared filesystem helpers still required by
  `kraai-open-files`, `kraai-close-files`, or `kraai-edit-file`; delete the rest
  with their callers.

Delete `kraai-tool-bash` completely. Arbitrary Nushell plus external command
execution replaces its model-facing purpose. Reusable command-output and timeout
behavior belongs in the sandbox/Nushell runner rather than a compatibility
wrapper.

### `kraai-toon-schema`

- Remove it from Kraai's workspace, manifests, dependency graph, generated
  `Cargo.nix`, checks, examples, and documentation once no consumer remains.
- Do not create a separate repository or preservation migration as part of this
  work. The source remains recoverable from commit history.
- Do not keep a local compatibility dependency after the cutover.

### `kraai-command-runner`

Preserve:

- Fail-closed sandbox setup, Bubblewrap capability probing, protected metadata
  mounts, restricted-network enforcement, process-tree termination, and timeout
  ownership.
- The ability to construct platform-appropriate read-only runtime dependencies,
  but represent those dependencies as explicit generic sandbox input.

Replace or add:

- The argv-only `CommandRequest` abstraction with a lower-level sandboxed
  process request that can safely launch the Nushell execution boundary.
- A dedicated framed duplex control channel. The Linux path keeps stdin for the
  seccomp filter and uses a socket inside private temp, connected only through a
  dedicated descriptor allowed by the restricted seccomp program.
- Complete output capture with no compaction, truncation, or overflow-artifact
  feature in this redesign.
- Clear separation between sandbox preparation and Nushell-specific execution.
- Do not teach the generic runner to infer Nix store paths or assume an FHS
  layout. Put optional Nix store, daemon, mount, and environment integration
  behind an explicit configuration flag owned by platform/package
  configuration.

Replace this crate with the confirmed `kraai-sandbox` boundary. Its internal
platform/process split does not depend on the later Nushell hosting decision.

### `kraai-agent`

Gut or replace:

- TOON protocol prompt text and generated schema concatenation.
- Tool-call parsing, allowed-tool lookup, typed preparation, per-tool assessment,
  pending-call queues, approval state, and prepared-call snapshots.
- Profile fields named `tools` and `default_risk_level` in their current meaning.
  Profiles instead select Kraai commands, a sandbox permission set, pre-policy
  permission rules, and the three-state escalation-policy enum.
- Replace the current built-in profile definitions with the agreed plan and
  coding command/capability sets. Keep command availability independent from
  temporary capability escalation so plan-mode workspace write approval does
  not expose `kraai-edit-file`.
- Parse-failure messages that refer to TOON or named tool DTOs.

Preserve and integrate:

- Turn ownership, active-profile snapshots, durable history writes,
  continuation preparation, and recovery after partial streams.
- Opened-file snapshot reconstruction and prompt injection.
- Workspace `AGENTS.md` prompt loading.

Add:

- Concise Nushell execution guidance and generated Kraai-command signatures.
- Script extraction results and durable execution state suitable for runtime
  scheduling.
- Whole-script escalation eligibility and approval before execution begins.
- Profile permission-rule evaluation before fallback escalation policy.
- A defined rule for when state changes from nested commands become visible to
  the current script, later commands, persistence, and the next model turn.

### `kraai-runtime`

Gut or replace:

- Default `ToolManager` construction and direct dependencies on every tool
  implementation.
- `handle_execute_tools`, prepared-call batches, per-tool approval dispatch,
  `ToolBatchOutcome`, pending-tool queries, and manual continuation behavior in
  their current form.
- `ToolCallDetected` and `ToolResultReady` event payloads based on tool ID and
  JSON arguments.
- Tests whose only purpose is the old approval queue or multi-TOON-call batching.

Preserve and adapt:

- Stream completion/cancellation, session isolation, queue draining, durable
  continuation ordering, event broadcasting, and process-task ownership.
- The streaming guard's responsibility to stop visible generation at an
  executable block, while replacing TOON-specific assumptions with a robust raw
  script boundary.

Add:

- A single script-execution task per accepted `<tool_call>` block.
- Incremental delivery and persistence of ordinary assistant preamble text
  before the opening tag, followed by a transition into script capture.
- One pre-execution approval path for a complete escalated script; no approval or
  scheduling boundary exists between Nushell statements.
- Immediate provider-stream cancellation when the first closing `</tool_call>`
  tag is detected, with trailing bytes discarded rather than persisted.
- Durable ingestion of successful stateful-command effects while the script is
  running so later failure or cancellation does not erase completed operations.
- Script lifecycle events, cancellation, complete result collection, state-event
  ingestion, persistence, and one continuation decision.
- Parent execution and nested operation observability without exposing internal
  envelopes to the model.

### `kraai-persistence`

Preserve:

- Durable message ancestry, atomic writes, tool-state snapshots, and replayable
  state deltas.

Replace or clarify:

- Replace per-tool `ChatRole::Tool` messages and duplicate `[Tool Result]`
  formatting with one provider-neutral `ToolCallResult` message containing the
  agreed `<tool_call_result>` block for the whole script.
- Whether raw script source remains solely in the assistant message or gains a
  structured persisted execution record.
- How partial output, cancellation, runtime failure, and nested state changes are
  represented across restart.

TOON-era sessions are intentionally incompatible. Delete legacy decoding,
fallback loading, migration, and specialized historical rendering; the
redesigned runtime starts new sessions with the new persisted execution model.

### `kraai-tui`

Gut or replace:

- TOON decoding and tool-name/argument cards.
- `PendingTool`, pending-tool polling, and tool-ID/argument-based approval cards.
- Per-tool approval state and UI branches keyed to tool risk levels.
- UI branches keyed to `ToolBatchOutcome` and per-tool result events.

Add:

- Syntax-aware or clearly delimited Nushell script cards.
- Running, completed, failed, cancelled, timed-out, sandbox-denied, and
  permission-denied execution states.
- A script-source approval card shown only for eligible escalation requests.
- Complete output presentation that distinguishes the final Nushell output from
  diagnostic stdout/stderr where the runtime exposes both.
- Exact output presentation without token-saving compaction, summarization, or
  silent truncation.
- Nested operation details only if they materially improve debugging; the normal
  chat view should remain uncluttered without altering result content.

### Provider crates

No provider-native tool-calling work is planned. Providers continue streaming
assistant text. Provider message conversion preserves assistant `<tool_call>`
source and maps the internal result role to exact `<tool_call_result>` text
without interpreting Nushell or adding another wrapper.

### `kraai-eval`

Add a comparison suite that records:

- Tool-related system-prompt tokens.
- Model-authored invocation tokens.
- Tool-result tokens returned to context.
- Number of execution round trips and continuation calls.
- Nushell parse/runtime failures and correction attempts.
- Task success, latency, output volume, and sandbox failures.
- Schema/example prompt cost and whether examples reduce malformed invocation
  or escalation syntax.

The old implementation must remain available long enough on a comparison branch
or fixture to establish a baseline, but it should not survive in the final
runtime as a feature flag.

### Workspace Cargo and Nix

- Pin the Nushell crate family and package `kraai-nushell-host` plus the matching
  clean `nu` used for development/conformance; package no Kraai helper or plugin
  binary.
- Ensure the application wrapper provides the intended general command PATH,
  including `rg`, without inheriting user Nushell configuration.
- Add Nushell to the development shell and relevant test derivations.
- Update workspace members and dependencies as old tool crates are removed or
  regrouped.
- Regenerate `Cargo.nix` through the existing maintenance command.
- Keep `just check` as the final repository gate.

## High-Confidence Deletion Targets

The following should not exist after the cutover:

- Model-authored TOON or JSON tool arguments.
- `toon_tool!` use anywhere in the Kraai repository.
- `toon-format` and `kraai-toon-schema` dependencies.
- `kraai-tool-bash`.
- `kraai-tool-read-file`, `kraai-tool-list-files`, and
  `kraai-tool-search-files`.
- TOON parsing or display code.
- The current named-tool preparation pipeline.
- Old per-tool schemas in the system prompt.
- A hidden compatibility mode that silently accepts both protocols.

The replacement crate layout and selected dedicated-child Nushell boundary are
recorded in [03-target-crate-architecture.md](03-target-crate-architecture.md)
and [04-nushell-hosting-decision.md](04-nushell-hosting-decision.md). The
capability, environment, persistence-status, profile, and process contracts are
inputs to implementation sequencing rather than decisions to reopen implicitly.
