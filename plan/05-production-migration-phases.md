# Production Migration Phases

> Status: implementation-sequencing draft. The product contracts, crate
> boundaries, and embedded-child hosting decision are inputs. The feasibility
> work in this plan is production code with an early acceptance gate, not a
> disposable prototype.

## Production-Shaped Means

Every new component starts in its intended final crate and process boundary.
Early phases may leave the old runtime as the active caller while the new path
is assembled, but the new path must not be implemented as a temporary binary,
test-only architecture, alternate protocol, or compatibility feature flag.

Production-shaped work requires from its first phase:

- Final ownership and dependency direction from
  [03-target-crate-architecture.md](03-target-crate-architecture.md).
- Typed requests, outcomes, and errors rather than stringly connected layers.
- Fail-closed sandbox and capability behavior.
- Cancellation and complete process-tree ownership.
- Streaming output without unbounded intermediate collection introduced by
  Kraai.
- Structured tracing keyed by script execution and command invocation IDs.
- Unit, integration, failure-path, and platform-appropriate sandbox tests.
- Exact dependency pins and Nix packaging updated with the Rust workspace.
- No plugin, generated-helper, external-Nushell, or old-protocol fallback.

Test fixture commands may exercise the native command platform, but they live in
test support and are never registered in a production profile or binary.

## Migration Shape

The implementation is constructed incrementally, but the user-facing protocol
has one cutover:

```text
production foundations built behind stable local contracts
  -> embedded host acceptance gate passes
  -> native commands and persistence path become complete
  -> runtime, prompt, profiles, and TUI switch together
  -> old tools and TOON are deleted
```

There is no released dual-protocol period. Temporary coexistence in the working
tree exists only so normal workspace checks can stay green while final
components are introduced. New code must not call back into legacy tool traits,
TOON schemas, or named-tool preparation.

Each phase ends with focused tests and `just check`. Phases that alter Nix
outputs or runtime closures also run the relevant Nix build/check derivations.
`Cargo.nix` is regenerated whenever the settled workspace graph changes.

## Phase 0: Capture The Baseline Before Deletion

### Purpose

Preserve measurements needed to determine whether the redesign improves token
efficiency and reliability without retaining the old runtime as compatibility
code.

### Work

- Record the exact repository revision used as the final TOON baseline.
- Capture system-prompt token counts for the built-in plan and coding profiles.
- Capture invocation and returned-result tokens for representative bash,
  read/search, edit, and open/close workflows.
- Run the current open/close evaluation suite with fixed model, provider, and
  attempt metadata.
- Record syntax failures, retries, round trips, task success, wall time, output
  volume, and sandbox failures.
- Store stable comparison fixtures and measurement results in `kraai-eval` or
  evaluation artifacts without preserving an executable TOON compatibility
  path.
- Document how the baseline is regenerated from repository history if another
  comparison is needed later.

### Exit gate

- The baseline revision and environment are identifiable.
- Token and behavioral measurements can be compared with the future Nushell
  path.
- Removing TOON crates will not remove the only copy of the comparison data.

## Phase 1: Extract The Production Sandbox Boundary

### Purpose

Turn the current 1.3k-line `kraai-command-runner` into the generic process and
isolation boundary required by the embedded host. This is a move and redesign of
the surviving sandbox logic, not a wrapper around the god file.

### Crates and modules

Create `kraai-sandbox` with final-purpose modules such as:

```text
src/
  lib.rs
  config.rs
  capabilities.rs
  launch.rs
  process.rs
  output.rs
  error.rs
  platform/
    mod.rs
    linux/
      mod.rs
      bubblewrap.rs
      seccomp.rs
      probe.rs
```

### Work

- Move Bubblewrap discovery, probing, mount construction, network restrictions,
  protected metadata, and process-group termination into cohesive modules.
- Replace `CommandRequest` with a generic launch plan capable of starting an
  absolute executable with explicit arguments, environment, current directory,
  stdio, runtime roots, effective capabilities, and narrowly declared private
  IPC descriptors.
- Replace the old sandbox-mode/escalated-command vocabulary with the agreed
  capability set and resolved sandbox permission set.
- Support `workspace-read`, `host-read`, `workspace-write`, `metadata-write`,
  `host-write`, `network`, and exclusive `no-sandbox` semantics.
- Keep workspace root as the initial working directory and create one private
  writable temporary directory per execution.
- Make live stdout/stderr delivery part of the process contract instead of
  always reading both streams to completion before returning.
- Preserve an authoritative final capture containing every emitted byte.
- Keep Nix paths, daemon access, and store behavior out of generic policy;
  optional Nix additions enter as explicit configuration and runtime roots.
- Preserve `#![forbid(unsafe_code)]` where possible. Use safe OS abstractions for
  private transport setup; if future process setup cannot be expressed safely,
  isolate the minimum reviewed code behind a narrow module rather than weakening
  the whole workspace.
- Remove `kraai-command-runner` after its remaining callers use
  `kraai-sandbox`; do not retain it as an alias crate.

### Tests

- Capability closure maps to the expected read/write/network boundary.
- Workspace metadata remains protected under ordinary workspace write.
- Host write permits writes anywhere visible to the sandbox.
- Network capability covers IP networking and visible Unix-domain sockets.
- Restricted execution fails closed when Bubblewrap or required namespaces are
  unavailable.
- Timeout and cancellation kill nested descendants and close output streams.
- Private temporary storage is writable, isolated, and removed after the tree
  exits.
- Runtime roots are readable without becoming writable.
- `no-sandbox` still uses process-tree ownership and timeout enforcement.

### Exit gate

- Existing sandboxed command behavior is preserved or deliberately replaced by
  the confirmed capability contract.
- The generic launcher can carry the private channels required by the Nushell
  host.
- No Nushell or model-protocol type appears in `kraai-sandbox`.

## Phase 2: Build The Embedded Host As Production Code

### Purpose

Prove the selected hosting architecture using the real runtime crate, host
binary, IPC contracts, engine construction, and failure model that will ship.

### Crates

- Add `kraai-command-core` with the final command declaration, registration,
  context, and effect-client abstractions.
- Add `kraai-nushell-runtime` with its parent-side library and the
  `kraai-nushell-host` binary.
- Add the new execution IDs, request values, statuses, and capability values to
  `kraai-types` only when they are shared across crate boundaries.
- Pin the complete Nushell crate family to one exact full version and add the
  matching clean `nu` executable to development and conformance-test inputs.

### Parent side

- Construct one immutable effective execution request.
- Create separate request, stdout, stderr/diagnostic, event, and acknowledgment
  channels.
- Generate a per-execution authentication secret and sequence state frames.
- Launch the absolute host executable through `kraai-sandbox`.
- Own timeout, cancellation, process-tree termination, and lifecycle evidence.
- Stream output live while retaining the complete final result.

### Host side

- Consume and close the one-shot request channel before parsing model source.
- Construct a fresh Nushell engine from the pinned language and normal command
  contexts.
- Apply the explicit environment, PATH, workspace root, temp directory, and
  clean-startup settings.
- Load no user config, hooks, history, autoload directory, or plugin registry.
- Register only commands selected in the immutable request.
- Parse the exact supplied source bytes with an internal source name and no
  prepended definitions.
- Evaluate exactly one program and exit after all owned work terminates.
- Return native values and streams directly within Nushell pipelines; ordinary
  command values never travel through parent IPC.

### Production test support

Add test-only native commands through the real `kraai-command-core` contract:

- A command returning a structured record.
- A lazy command producing a stream of structured records.
- A stateful command emitting an authenticated durable effect.
- A command that launches a nested external process for cancellation tests.

These commands validate production extension points without becoming part of a
profile or runtime registry.

### Acceptance tests

- Native records pass through `where`, `select`, `get`, `first`, and aggregation
  without text serialization.
- Partial downstream consumption stops unnecessary upstream production.
- Cancellation reaches a running native command and every external descendant.
- External programs behave like the matching clean pinned Nushell runtime.
- Environment, current directory, pipeline, diagnostic span, and final-value
  rendering match the promised clean-startup behavior.
- A completed state effect is authenticated, persisted, and acknowledged before
  its success result is exposed.
- A later error, timeout, or forced termination does not erase an acknowledged
  effect.
- Script stdout/stderr cannot forge control frames.
- External descendants cannot inherit a usable control channel, trace the host,
  or read its authentication secret.
- Invalid source is reported without executing any part of the script.
- Host startup, engine initialization, sandbox establishment, and execution
  errors map onto distinct stable outcomes.
- Cold-start time, time to first output/item, binary size, and runtime closure
  size are recorded.

### Stop condition

If the embedded host cannot meet sandbox, pipeline, cancellation, diagnostic,
or state-durability requirements, stop the migration and return the architecture
for explicit review. Do not implement the plugin or generated-helper designs as
a fallback.

## Phase 3: Introduce The Script Protocol And Policy Model

### Purpose

Replace regex/TOON framing and numeric risk assessment with the final
model-facing block protocol and capability policy before connecting it to the
agent runtime.

### Work

- Add `kraai-script-protocol` as a standalone crate.
- Implement a byte-stream state machine for leading assistant text, one opening
  tag, attributes, exact script bytes, and the first closing tag.
- Require a Nushell-style timeout attribute with no default or maximum.
- Parse optional comma-separated capability additions from `permissions`.
- Reject unknown attributes, unknown capabilities, duplicates, empty entries,
  malformed durations, incomplete tags, and a second block.
- Expose the point at which the provider must be cancelled and trailing bytes
  discarded, including bytes already received in the same chunk.
- Implement capability closure/subsumption once in `kraai-types`.
- Add per-capability `Deny`/`Prompt`/`Allow` rules and the fallback
  `EscalationPolicy` enum.
- Implement aggregate resolution with deny precedence, at most one approval,
  and semantic no-op requests for already granted capabilities.
- Add final plan and coding profile values, including command availability,
  sandbox defaults, environment allow-list, PATH behavior, and clean Nushell
  startup.

### Tests

- Every delimiter and attribute split across provider chunks.
- Leading assistant prose streamed and persisted exactly once.
- Closing tag followed by newline, prose, or a second block in the same chunk.
- Unicode and arbitrary Nushell bytes preserved without rewriting.
- Required timeout validation and no hidden default/maximum.
- Capability closure, `no-sandbox` exclusivity, rule precedence, and one-shot
  prompt aggregation.
- Denied scripts cannot construct an executable request.

### Exit gate

- Framing is independent of Nushell parsing and runtime execution.
- Policy resolution returns one immutable effective request or a denial without
  inspecting Nushell source.
- No old risk level or per-tool assessment participates in the new path.

## Phase 4: Build The Initial Native Command Set

### Purpose

Move only the filesystem and context behavior that survives the redesign into
final shared and per-command crates.

### Work

- Add `kraai-workspace-fs` and split path resolution, containment, reads, and
  edit validation/application into cohesive modules.
- Preserve symlink handling, UTF-8 checks, size limits, expected-text checks,
  overlapping-edit rejection, and atomic file replacement where applicable.
- Add one final implementation crate for each command:
  `kraai-command-open-files`, `kraai-command-close-files`, and
  `kraai-command-edit-file`.
- Implement the declarative `kraai-command-core` declaration that produces the
  native Nushell adapter, static prompt help, full examples, capability
  metadata, and registration metadata from one source.
- Make Nushell own argument parsing, flags, pipeline input, and structured
  output.
- Keep command execution inline with Nushell evaluation.
- Require runtime capability checks in addition to registry selection.

### Command-specific contracts

- `kraai-open-files` validates paths and durably pins each successful path for
  fresh context injection on future turns. It returns only status and normalized
  path metadata, never file contents.
- `kraai-close-files` durably removes successfully closed paths from future
  context injection and reports status/path metadata.
- `kraai-edit-file` preserves validated edit/create behavior and returns a
  structured operation result after the filesystem operation completes.
- Immediate file inspection remains normal Nushell `open` or an external
  command such as `cat`; list/read/search are not recreated as Kraai commands.

### Tests

- Compile-time prompt metadata matches the native command signature and
  examples.
- Unregistered commands are absent, and registered commands still reject a
  missing effective capability.
- Open/close effects are acknowledged durably and survive a later statement
  failure, timeout, and runtime restart.
- Opening several paths records every successful path according to actual
  execution, without pretending later failures roll earlier effects back.
- Open-files output contains no file contents.
- Edit behavior retains all surviving correctness tests from the current tool.
- Structured results compose with ordinary Nushell pipeline stages where the
  command contract exposes data.

### Exit gate

- New command crates have no dependency on agent, runtime, persistence, TUI,
  TOON, or old tool traits.
- Shared filesystem code contains no Nushell, prompt, profile, or state-effect
  policy.
- Adding a future command requires a new implementation crate and registry
  entry, not editing a central command god file.

## Phase 5: Replace Persistence And Recovery Models

### Purpose

Persist scripts and their effects directly instead of adapting them into named
tool calls and batches.

### Work

- Add durable script execution records keyed by `ScriptExecutionId`.
- Persist the exact assistant preamble and `<tool_call>` source as message
  content.
- Persist requested/effective capabilities, profile snapshot identity, timeout,
  timestamps, stable status, exit detail, diagnostics, and complete output.
- Persist context effects as authenticated events arrive, before final script
  completion.
- Acknowledge an effect only after the corresponding persistence transaction is
  durable.
- Reconstruct opened-file context from the renamed state snapshot/effect model.
- Recover interrupted executions into an explicit terminal result without
  replaying arbitrary scripts after restart.
- Split message, execution, context-state, and turn storage into separate
  modules rather than enlarging persistence god files.
- Remove old tool result DTOs, state-delta naming, and format helpers from the
  new model. No TOON-era session migration or fallback reader is added.
- Add a distinct provider-neutral `ToolCallResult` history role.
- Render one `<tool_call_result>` block per script from structured status and
  separately persisted output channels. Include only nonempty stdout/stderr
  sections and never add a second result prefix in provider adapters.
- Keep the rendered block out of the executable script parser; result contents
  remain inert even when they contain strings resembling protocol tags.

### Tests

- Crash after acknowledged effect but before final status.
- Crash during output streaming and recovery of the complete persisted prefix.
- Later script failure does not erase earlier acknowledged context effects.
- A recovered execution cannot schedule continuation twice.
- Old TOON sessions are rejected clearly rather than partially decoded.
- Every stable status renders the required result attribute.
- Exit code and stdout/stderr sections appear only when applicable.
- Output containing `<tool_call>`, `</tool_call_result>`, or other result-like
  text is preserved exactly and remains inert.
- The persisted structured channels can reproduce the model-facing result block
  without consulting transient runtime state.

## Phase 6: Perform The Agent And Runtime Cutover

### Purpose

Replace named-tool preparation, batches, and per-tool approvals with one script
state machine.

### Work

- Stream and persist ordinary assistant preamble while feeding the protocol
  parser.
- Stop the provider at the first complete closing tag and discard trailing
  bytes.
- Resolve profile command availability, requested capabilities, per-capability
  rules, and fallback escalation policy.
- Show at most one allow-once/deny-once approval for the whole exact script.
- Start the immutable request only after approval, with timeout beginning
  immediately before host launch.
- Stream complete output and context effects into persistence while execution is
  live.
- Persist exactly one terminal status.
- Continue the model exactly once for every agreed terminal status except
  user-initiated cancellation.
- Map the internal `ToolCallResult` role to the exact rendered result block in
  every provider adapter without using provider-native tool APIs.
- Keep provider transport code unaware of Nushell syntax beyond runtime-driven
  stream cancellation.
- Replace the tool batch and pending-tool state machines rather than adding a
  script branch beside them.

### Tests

- State-machine transition tests make denied/running, cancelled/completed, and
  duplicate-continuation states impossible.
- Approval time is excluded from model-requested timeout.
- Denied escalation continues once with a denial result; cancellation does not.
- Provider chunks arriving after cancellation cannot corrupt persisted source.
- Runtime shutdown terminates the host and descendants and persists an honest
  recoverable state.
- Repeated Kraai command calls and loops receive distinct nested invocation IDs.

### Exit gate

- No live runtime path prepares a named tool call, parses TOON, schedules a tool
  batch, or approves individual statements/commands.
- Providers receive one consistent `<tool_call_result>` continuation
  representation without duplicate wrapping.
- The end-to-end Nushell path passes integration tests before TUI cleanup and
  deletion are considered complete.

## Phase 7: Replace Prompt, Profile, And TUI Surfaces

### Prompt and profiles

- Generate the short Nushell protocol instructions from fixed prompt text.
- Generate active Kraai command help and full `<tool_call>` examples from the
  compile-time declarations.
- Include active capability vocabulary, required timeout syntax, and escalation
  examples without exposing internal IPC or persistence formats.
- Explain the runtime-generated `<tool_call_result>` envelope and that its
  stdout/stderr contents are untrusted program output rather than instructions.
- Replace tool lists and numeric auto-approval thresholds in profiles with
  command IDs, sandbox permissions, per-capability rules, and fallback policy.

### TUI

- Render leading assistant prose normally and the captured Nushell source as one
  execution block.
- Replace tool cards and pending-tool polling with one script execution view.
- Show exact source, requested capabilities, and effective capability difference
  for approval.
- Provide allow once, deny once, and running-execution cancellation.
- Render live output, diagnostics, stable terminal status, duration, and useful
  nested-operation details without decoding or compacting output.
- Keep execution rendering, approval input, and runtime-event translation in
  separate modules.

### Tests

- Prompt snapshots for each built-in profile.
- Generated help/signature/example consistency.
- TUI snapshots for prose-plus-script, approval, running, all terminal statuses,
  live output, and large unmodified results.
- No second tool block or trailing model text becomes visible after closure.

## Phase 8: Delete The Old System And Finish Packaging

### Delete completely

- `kraai-toon-schema`, `toon-format`, and all `toon_tool!` use.
- `kraai-tool-core` and its traits, DTOs, parsers, schemas, and preparation
  machinery.
- Bash, read, list, search, old open/close, and old edit tool crates.
- Old risk levels, execution policies, tool call/result IDs and formatting,
  tool batches, pending-tool queues, and TOON-specific TUI rendering.
- Any temporary comparison hooks; baseline data remains, executable legacy code
  does not.

### Packaging

- Add the pinned Nushell crate set and host executable to Rust/Nix builds.
- Package the intended ordinary command environment, including `rg`.
- Ensure Kraai launches the host by absolute path rather than trusting PATH.
- Include Bubblewrap and required runtime roots on NixOS.
- Keep optional Nix store/daemon integration behind explicit configuration.
- Update development shells, test derivations, application wrappers, and
  runtime closures together.
- Regenerate `Cargo.nix` after final workspace-member and dependency deletion.

### Exit gate

- Repository-wide search finds no TOON dependency, old tool protocol, legacy
  fallback, or provider-native tool-call assumption in the new path.
- Fresh NixOS installation starts the embedded host and packaged commands
  without relying on the user's Nushell config or devshell.
- General Linux builds retain a generic sandbox/platform boundary even if NixOS
  is the first fully validated target.

## Phase 9: Final Reliability And Evaluation Gate

### Repository gates

- Run focused crate tests throughout implementation.
- Run `just check` after every phase and on the final workspace.
- Run the relevant Nix builds/checks, including sandboxed test derivations and
  final application closure tests.
- Test from a clean workspace and a fresh persistent-data location.

### Fault and lifecycle matrix

- Invalid opening tag, timeout, permission request, and Nushell source.
- Sandbox unavailable before launch.
- Host missing, engine initialization failure, and command registration failure.
- Parent or child channel closure at each state-effect handshake step.
- Persistence failure before and after acknowledgment.
- User cancellation during native work and nested external processes.
- Timeout after partial output and after an acknowledged context effect.
- Runtime restart during streaming, approval, execution, final persistence, and
  continuation scheduling.

### Comparative evaluation

- System prompt, invocation, and returned-result tokens.
- Task success and correction attempts.
- Number of model round trips.
- Nushell parse/runtime failure rates.
- Host cold start, time to first output/item, and full execution latency.
- Streaming throughput and partial-consumption behavior.
- Runtime closure and binary size.
- Sandbox and cleanup failures.
- Opened-file context correctness across continuation and restart.

Performance thresholds should be set from the Phase 0 baseline and Phase 2 host
measurements rather than invented before data exists. Correctness, isolation,
durability, and complete result fidelity remain release blockers even if token
or latency measurements improve.

## Explicitly Outside These Phases

- Persistent approval rules or “allow always” interaction.
- A maximum/default model-requested timeout.
- Output truncation, summarization, compaction, or overflow artifacts.
- Persistent host pooling or shared Nushell engine state.
- Per-script Nix devshell/direnv execution beyond preserving the environment
  provider seam.
- macOS or Windows sandbox implementation.
- Plugins, generated Nushell wrappers, or helper-process fallbacks.
- TOON/session compatibility or migration.

## Completion Definition

The migration is complete only when the embedded host is the sole executable
tool surface, the old tool and TOON system is deleted, NixOS packaging and
sandbox tests pass, stateful context behavior survives failures and restarts,
and comparative evaluation data exists. A passing host experiment without the
runtime, persistence, TUI, deletion, packaging, and reliability work is not a
completed implementation.
