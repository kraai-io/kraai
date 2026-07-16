# Target Crate Architecture

> Status: architectural draft. The product and execution contracts in
> [02-open-decisions.md](02-open-decisions.md) are inputs to this design. This
> document uses the selected dedicated-child embedded Nushell architecture.

## Goals Of The Split

- Give the model-facing protocol, policy, sandbox, Nushell engine, Kraai command
  system, persistence, and turn lifecycle exactly one owner each.
- Keep platform isolation generic. NixOS integration may configure the generic
  boundary but must not leak Nix assumptions into it.
- Prevent model-authored source from being parsed or rewritten anywhere except
  the protocol boundary and Nushell itself.
- Preserve stateful opened-file context without letting trusted commands bypass
  the effective sandbox capabilities.
- Delete the old tool abstraction rather than renaming `TypedTool` and carrying
  its DTO/schema lifecycle forward.
- Break current large files into cohesive modules so a future change does not
  require loading the entire execution stack into context.

## Proposed Workspace Shape

```text
crates/
  kraai-types/                 shared durable value types
  kraai-script-protocol/       streaming <tool_call> framing only
  kraai-sandbox/               generic process isolation and ownership
  kraai-nushell-runtime/       one-script Nushell execution boundary
  kraai-command-core/          native Kraai command contract and metadata
  kraai-command-open-files/    opened-file context command
  kraai-command-close-files/   close opened-file context command
  kraai-command-edit-file/     validated workspace editing command
  kraai-workspace-fs/          reusable path, read, and edit domain logic
  kraai-agent/                 profiles, prompts, context, turn decisions
  kraai-persistence/           durable messages, executions, and state effects
  kraai-runtime/               application orchestration and provider lifecycle
  kraai-tui/                   user interaction and rendering
  kraai-eval/                  protocol, token, and reliability evaluation
  llm-providers/               provider integrations
```

Confirmed foundational crate boundaries:

- `kraai-script-protocol` is a standalone workspace crate, not a module inside
  `kraai-runtime` or the Nushell execution crate. Its security-sensitive
  streaming parser and framing tests remain isolated from turn orchestration.
- `kraai-sandbox` replaces the generic command runner and owns isolation without
  Nushell knowledge.
- `kraai-nushell-runtime` owns one whole script execution even while its internal
  engine is embedded in a dedicated sandboxed child.
- `kraai-command-core` owns command contracts and trusted bridging without
  containing command implementations.
- `kraai-workspace-fs` is a pure shared domain crate for filesystem mechanics;
  it contains no command registry, Nushell integration, profile policy, or state
  effect ownership.

Other names remain recommendations, but the ownership boundaries are
requirements.

## Dependency Direction

`A -> B` means crate A depends on crate B:

```text
kraai-script-protocol -> kraai-types
kraai-sandbox -> kraai-types
kraai-command-core -> kraai-types
kraai-command-open-files -> command-core, workspace-fs, types
kraai-command-close-files -> command-core, workspace-fs, types
kraai-command-edit-file -> command-core, workspace-fs, types
kraai-nushell-runtime -> sandbox, command-core, individual commands, types
kraai-persistence -> kraai-types
kraai-agent -> kraai-command-core, kraai-persistence, kraai-types
kraai-runtime -> protocol, sandbox, nushell, commands, agent, persistence
kraai-tui -> kraai-runtime, kraai-types
```

This is not a complete Cargo graph; it describes authority direction:

- `kraai-runtime` composes the lower layers but lower layers never depend on it.
- Command implementations do not depend on the agent, runtime, persistence, or
  TUI.
- `kraai-nushell-runtime` is the composition point that links individual
  command crates into the host binary; profile selection still controls which
  linked commands are registered for a particular script.
- The sandbox does not depend on Nushell or understand `<tool_call>`.
- The protocol crate does not execute scripts or evaluate permissions.
- Persistence stores shared types but does not decide continuation.
- Providers remain unaware of the executable protocol; they stream text and
  honor cancellation.

## Shared Contracts

### Script request

The accepted request contains:

- Exact raw Nushell source, without injected prefixes or rewritten commands.
- Required model-authored timeout.
- Requested capability additions after syntax validation but before policy.
- Parent assistant message identity and a new script execution identity.
- Workspace identity needed to resolve the fixed initial directory.

Leading assistant prose stays in the assistant message. It is not copied into
the Nushell source.

### Effective execution request

After profile evaluation and any one-shot approval, the runtime constructs:

- Exact script request.
- Profile snapshot used for the decision.
- Effective capability closure.
- Environment, PATH, and clean-startup selections.
- Generic read-only runtime roots.
- Private temporary-directory configuration.
- Optional, configuration-guarded platform integrations.

This immutable value is the input to Nushell execution. Policy is never
re-evaluated inside the child process.

### Execution events

One event vocabulary crosses the execution boundary:

- Script started.
- Output produced.
- Kraai command invocation started/completed.
- Context state effect completed.
- Process exited.
- Process tree killed.
- Final stable status.

Events carry a script execution ID. Nested Kraai command events also carry a
command invocation ID allocated by trusted runtime code. Model-authored Nushell
must not be able to forge either identity or a state-effect event.

## Crate Plans

### `kraai-types`

Keep this crate dependency-light and split its current `lib.rs` into modules:

```text
src/
  chat.rs
  ids.rs
  profiles.rs
  permissions.rs
  script.rs
  context_state.rs
  usage.rs
```

Add:

- `ScriptExecutionId` and `CommandInvocationId` newtypes.
- A provider-neutral `ToolCallResult` history role and structured script-result
  values separate from human-authored user messages.
- `SandboxCapability` with `WorkspaceRead`, `HostRead`, `WorkspaceWrite`,
  `MetadataWrite`, `HostWrite`, `Network`, and `NoSandbox`.
- Semantic capability closure and subsumption in one implementation.
- `SandboxPermissionSet`, per-capability rule types, and `EscalationPolicy` with
  `Deny`, `Prompt`, and `Allow`.
- Profile environment, PATH, and Nushell-startup enums.
- Script metadata, requested/effective execution values, stable execution
  status, and durable result types.
- Renamed context state snapshot/delta types that are not tied to the removed
  generic tool abstraction.

Delete:

- `ToolCall`, `ToolId`, `ToolResult`, `RiskLevel`, `ExecutionPolicy`,
  `ToolCallAssessment`, and `SandboxPermissions` in their current forms.
- Formatting helpers that encode TOON or old named-tool result messages.

Do not put parsing, process execution, prompt rendering, or persistence logic in
this crate.

### `kraai-script-protocol` (confirmed new crate)

Own only model-facing framing:

```text
src/
  parser.rs
  start_tag.rs
  duration.rs
  error.rs
```

Responsibilities:

- Incrementally pass through assistant preamble text.
- Recognize a start tag across arbitrary provider chunk boundaries.
- Parse only the known `permissions` and required `timeout` attributes.
- Capture the raw body without interpreting Nushell.
- Recognize the first complete closing tag, emit a complete script request, and
  tell the runtime to cancel the provider stream immediately.
- Discard bytes after that closing tag, including bytes in the same chunk.
- Reject malformed tags, unknown attributes/capabilities, missing timeouts,
  duplicate capability names, empty entries, and `no-sandbox` combinations.
- Produce `invalid-script` protocol diagnostics without attempting execution.

It must be tested with every delimiter split across chunk boundaries, leading
prose, same-chunk trailing data, incomplete streams, multibyte UTF-8 boundaries,
and adversarial attribute ordering. It must not use regex as the protocol
authority.

### `kraai-sandbox` (confirmed replacement for `kraai-command-runner`)

This crate owns generic isolation and process-tree lifetime, not Nushell.

```text
src/
  lib.rs
  request.rs
  output.rs
  process_tree.rs
  temp_dir.rs
  platform/
    mod.rs
    linux/
      mod.rs
      bubblewrap.rs
      mounts.rs
      seccomp.rs
      probe.rs
```

Responsibilities:

- Accept an executable, argv, fixed working directory, environment, descriptors,
  timeout, capability closure, runtime roots, and private temp configuration.
- Fail closed if the requested restrictions cannot be established.
- Keep protected workspace metadata read-only under `workspace-write`.
- Implement `host-read`, `host-write`, network, and `no-sandbox` exactly as the
  shared capability contract defines.
- Treat network as IP plus visible Unix-domain socket communication without
  mounting socket paths itself.
- Own the entire descendant process tree through completion, timeout, or
  cancellation.
- Provide private writable temp storage without granting filesystem write
  capabilities.
- Capture all process output without compaction or truncation.

Generic request types contain no Nix paths or daemon behavior. Nix integration
resolves runtime roots and optional mounts before calling this crate and is
enabled only through explicit configuration.

The current 1.3k-line `lib.rs` should not survive the move; platform setup,
process ownership, seccomp, and request/result types become separate modules.

### `kraai-command-core` (confirmed replacement contract)

Own the native Kraai command contract:

```text
src/
  command.rs
  context.rs
  metadata.rs
  registry.rs
  events.rs
  docs.rs
```

Responsibilities:

- Declare a command's name, description, examples, native Nushell signature,
  pipeline input/output shape, streaming behavior, and required effective
  capabilities once.
- Generate concise prompt documentation and complete `<tool_call>` examples at
  compile time from that declaration.
- Register only commands selected by the active profile.
- Provide a trusted execution context containing scoped filesystem access,
  effective capabilities, workspace identity, and a state-event sink.
- Allocate unforgeable nested invocation IDs and emit authenticated, sequenced
  state effects that wait for durable parent acknowledgment before success.

Nushell parses source and binds arguments/pipeline input. This crate receives the
native bound invocation and never parses model-authored source or JSON/TOON DTOs.
It invokes the command immediately as part of Nushell evaluation and returns
native `PipelineData` directly to the engine. It must not route ordinary command
values through the parent-side state-event channel or eagerly collect streams.

Delete `TypedTool`, `ErasedTool`, `PreparedToolCall`, `ToolManager`, TOON schema
generation, named-tool lookup, risk assessment, and the TOON parser.

The single-source command declaration must generate both a native Nushell
`Command` adapter and static prompt metadata. Prefer a declarative macro in
`kraai-command-core`; do not create a proc-macro crate unless the implementation
proves that a declarative macro cannot express the required signatures and
examples cleanly.

### `kraai-workspace-fs` (confirmed new pure domain crate)

Extract reusable filesystem behavior from `kraai-tool-core` and the current
1.1k-line edit tool:

```text
src/
  path.rs
  read.rs
  containment.rs
  edit/
    mod.rs
    model.rs
    validate.rs
    apply.rs
```

Preserve containment, symlink, file-size, UTF-8, line-numbering, expected-text,
overlap, and atomic-write behavior with focused tests. Remove read/list/search
helpers that have no surviving caller.

This crate accepts explicit filesystem scope/roots rather than reading profile
policy. It contains no Nushell, prompt, agent, persistence, or TUI code.

### Individual Kraai command crates

Keep one implementation crate per model-facing Kraai command:

- `kraai-command-open-files` implements `kraai-open-files`, reads permitted
  paths to validate them, and emits one durable opened-file state effect per
  successfully opened path. Its immediate pipeline result exposes only status
  and normalized path metadata, never file contents; contents are freshly read
  for injection into future model turns.
- `kraai-command-close-files` implements `kraai-close-files` and emits one
  durable removal effect per successfully closed path.
- `kraai-command-edit-file` implements `kraai-edit-file`, applies the preserved
  edit semantics, and requires effective workspace write access for workspace
  paths.

Each crate exposes one native command declaration, lets Nushell own invocation
parsing, pipeline binding, backpressure, and downstream consumption, and calls
`kraai-workspace-fs` only for shared domain behavior. Command-specific tests and
dependencies remain local.

The command platform supports rich and streaming output, but each command owns
its semantic output contract. Platform capability must not accidentally turn a
state-management command such as `kraai-open-files` into an immediate file-read
command that duplicates ordinary Nushell `open` or external `cat`.

This is the default rule for future commands, not just the initial three. A
substantial capability such as a subagent-backed web search receives its own
crate rather than being added to a broad `kraai-commands` or category crate.
Shared crates are created only for cohesive logic used by multiple commands;
they must not become alternate command registries or dumping grounds.

The existing `kraai-tool-open-file`, `kraai-tool-close-file`, and
`kraai-tool-edit-file` crates are replaced rather than retained under legacy
names. Their domain tests move to the corresponding command or shared
filesystem crate.

### `kraai-nushell-runtime` (confirmed new crate)

Own one whole Nushell program execution:

```text
src/
  lib.rs
  request.rs
  parent/
    mod.rs
    launch.rs
    lifecycle.rs
    output.rs
    events.rs
  host/
    mod.rs
    engine.rs
    command_bridge.rs
    environment.rs
    output.rs
    events.rs
src/bin/
  kraai-nushell-host.rs
```

Parent-side responsibilities:

- Receive one immutable effective execution request from `kraai-runtime`.
- Create private request, output, diagnostic, control, and acknowledgment
  channels.
- Ask `kraai-sandbox` to launch the absolute host executable and own its whole
  process tree.
- Stream complete output and durably persist acknowledged state effects while
  the host is alive.
- Remain authoritative for timeout, cancellation, sandbox establishment, and
  final lifecycle status.

Child-host responsibilities:

- Consume and close the one-shot request channel before parsing source.
- Build the selected clean/inherited startup and environment behavior.
- Install only the profile-selected Kraai commands before parsing exact model
  source.
- Hand the complete source to one Nushell invocation without statement-level
  scheduling or continuation.
- Route native command calls through the trusted bridge.
- Forward output and authenticated, sequenced state events while the process is
  alive, waiting for durable acknowledgment when a command changes Kraai state.
- Execute exactly one script and never accept another request.

The parent maps completion, parse failure, timeout, cancellation, sandbox
failure, and startup failure onto the stable status vocabulary. It does not
trust the child to override direct lifecycle and sandbox observations.

This crate depends on `kraai-sandbox` for process isolation and ownership.
`engine.rs` embeds the exactly pinned Nushell engine in a dedicated host binary;
there is no external helper or plugin implementation path.

### `kraai-agent`

Reshape around profiles, prompting, and conversational state rather than tool
preparation:

```text
src/
  manager/
    sessions.rs
    streaming.rs
    continuation.rs
  profiles/
    mod.rs
    permissions.rs
    environment.rs
  prompts/
    mod.rs
    nushell.rs
    commands.rs
  context/
    mod.rs
    opened_files.rs
```

Delete `manager/tool_calls.rs`, typed preparation, per-tool assessment, tool
batches, approval queues, and TOON prompt/schema assembly.

Add:

- Profile resolution for command names, default capabilities, capability rules,
  fallback policy, environment, PATH, startup behavior, and optional platform
  configuration.
- The agreed built-in plan and coding profiles.
- Prompt construction from the fixed protocol explanation, active capability
  vocabulary, required timeout examples, and generated active-command docs.
- Opened-file context reconstruction from renamed context state snapshots and
  deltas.

The agent may decide that a terminal execution result warrants continuation, but
it does not spawn Nushell or implement sandbox policy mechanics.

### `kraai-persistence`

Persist the new model directly with no legacy reader:

- Assistant preamble and raw `<tool_call>` source as streamed message content.
- Script execution ID, exact source reference, requested/effective capabilities,
  profile snapshot identity, timeout, timestamps, stable status, and complete
  result output.
- Completed context state effects as they arrive, before final script status.
- Enough data to reconstruct opened-file context after restart even when a later
  statement fails or the script is killed.

Split storage logic by durable aggregate rather than adding everything to
`lib.rs`:

```text
src/
  messages.rs
  executions.rs
  context_state.rs
  turns.rs
```

There is no TOON-era migration, fallback loader, inert rendering mode, or schema
compatibility branch.

### `kraai-runtime`

Own the application state machine:

```text
src/runtime/
  provider_stream.rs
  script_detection.rs
  approval.rs
  execution.rs
  continuation.rs
  recovery.rs
  dispatch.rs
```

Flow:

1. Stream and persist assistant preamble.
2. Feed provider bytes to `kraai-script-protocol`.
3. On the complete closing tag, cancel the provider and discard trailing bytes.
4. Resolve capability closure and profile rules.
5. Persist `denied` or obtain one-shot approval when required.
6. Build the immutable effective request and invoke `kraai-nushell-runtime`.
7. Persist output and completed context effects while execution is active.
8. Persist one final status.
9. Continue exactly once for every terminal status except user cancellation.

Replace `PendingToolInfo`, `ToolBatchOutcome`, and per-tool runtime events with
script approval/execution views and the stable status vocabulary. Keep provider
management, session dispatch, queueing, logging, and workspace watching outside
the script modules.

An explicit state machine must make illegal combinations unrepresentable: an
execution cannot be both awaiting approval and running, a denied script never
starts, and continuation cannot be scheduled twice.

### `kraai-tui`

Replace tool cards and risk-level UI with:

- Live rendering of ordinary assistant preamble.
- A Nushell source block once the opening tag begins.
- One approval view showing exact source, requested capabilities, and effective
  difference from the default sandbox.
- Allow-once and deny-once actions only.
- Running, completed, denied, invalid, timed-out, cancelled,
  sandbox-unavailable, and failed-to-start states.
- Complete output and Nushell diagnostics without TOON decoding.

Split execution rendering, approval input, and runtime event translation into
separate modules. Remove the `toon-format` dependency and every old tool-name,
argument, risk, batch, and pending-tool rendering branch.

### `kraai-eval`

Keep the evaluation sandbox independent from the agent sandbox while reusing
generic low-level process primitives only where ownership remains clear.

Add evaluations for:

- Prompt and invocation token counts against the final pre-removal TOON
  baseline.
- Multiple operations in one script and model-authored result filtering.
- Nushell syntax correction and invalid protocol recovery.
- Capability request correctness and approval behavior.
- Open/close state durability when a later statement fails.
- Timeout, cancellation, process-tree cleanup, and restart recovery.

Rewrite or replace TOON-specific open/close fixtures rather than retaining a
compatibility evaluator.

### Provider crates

No provider-native tool-call implementation is added. Providers continue to
stream assistant text and expose cancellation. On continuation they map Kraai's
internal `ToolCallResult` role to ordinary provider input text containing the
already rendered `<tool_call_result>` block. They do not synthesize native tool
calls, function-call IDs, or an additional result wrapper.

Provider tests must prove that a runtime cancellation at the complete closing
tag stops further delivery without corrupting the assistant message already
persisted, and that result normalization preserves the complete block exactly.

## Deleted Workspace Members

Delete completely:

- `kraai-toon-schema`
- `kraai-tool-core`
- `kraai-tool-bash`
- `kraai-tool-read-file`
- `kraai-tool-list-files`
- `kraai-tool-search-files`
- `kraai-tool-open-file`
- `kraai-tool-close-file`
- `kraai-tool-edit-file`

Their surviving responsibilities move to the target crates above; their old
traits, DTOs, schemas, examples, tests, and compatibility types do not.

## Workspace And Packaging Consequences

- Replace deleted workspace members and dependencies in the root `Cargo.toml`.
- Keep all dependency versions as full triples in workspace dependencies.
- Remove `toon-format`, TOON macro dependencies, and obsolete grep/search crates
  once no surviving code uses them.
- Add pinned Nushell and `rg` to the intended runtime PATH through Nix packaging.
- Keep generic runtime-root configuration independent from the optional Nix
  integration flag.
- Regenerate `Cargo.nix` through the existing maintenance command after the
  workspace graph settles.
- Update Nix outputs, development shells, checks, and runtime closures together;
  NixOS is the first acceptance platform.

## Architecture Acceptance Criteria

- No crate other than `kraai-script-protocol` parses `<tool_call>` framing.
- No Kraai crate parses model-authored Nushell source; Nushell is the sole
  language parser.
- No sandbox module imports Nushell command or protocol types.
- No command implementation imports agent, runtime, persistence, or TUI types.
- Capability closure and subsumption have one implementation.
- Stable execution statuses have one shared definition.
- Opened-file state effects survive later script failure and restart.
- The generic sandbox contains no unconditional Nix path or daemon assumption.
- No replaced god file is recreated under a new crate name.
- The workspace contains no TOON dependency or legacy tool protocol path.
