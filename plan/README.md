# Nushell Tool-Call Redesign

> Status: discovery draft. This is intentionally not implementation-ready yet.
> The high-level direction and removal surface are documented; foundational
> product and execution-boundary decisions remain open in
> [02-open-decisions.md](02-open-decisions.md).

## Objective

Replace Kraai's model-facing tool protocol with sandboxed Nushell programs.
The body of each `<tool_call>` block will be Nushell source code rather than a
TOON, JSON, or provider-native tool invocation.

```nu
<tool_call timeout="2min">
rg --json "ToolCall" crates
| lines
| each { from json }
| where type == "match"
| first 20
</tool_call>
```

The redesign has two inseparable goals:

1. Make tool use feel like writing a small program over structured data.
2. Reduce context use by removing model-authored serialization, shrinking tool
   documentation, combining multiple operations into one execution, and
   filtering results before they are returned to the model.

This is a replacement, not a compatibility layer. Kraai is still in its alpha
stage, so the final implementation should delete the old protocol instead of
maintaining parallel TOON and Nushell paths.

## Target Model-Facing Experience

The model receives:

- A short explanation that `<tool_call>` contains Nushell source.
- Concise signatures and documentation for the stateful or safety-oriented
  Kraai commands enabled by the current profile.
- Normal Nushell language, built-in command, and pipeline semantics.
- Structured values from Kraai commands that compose with native Nushell
  commands.

The model does not receive or author:

- TOON tool schemas.
- JSON argument objects.
- A synthetic `bash` tool envelope.
- Internal transport envelopes, operation identifiers, or state-delta formats.

Serialization may still exist behind the process boundary, but it is a private
runtime detail and must not leak into the model-facing syntax.

## Intended Runtime Flow

```text
provider text stream
    -> extract raw Nushell from <tool_call>
    -> create one script execution
    -> if escalation was requested, check profile policy and obtain approval
    -> start Nushell inside the configured sandbox
    -> load the permitted Kraai command surface
    -> execute native commands, external programs, and Kraai commands
    -> capture all output plus completed state changes
    -> persist the result and state changes
    -> continue the model turn once
```

The profile, permission, environment, result, persistence, and user-experience
contracts were made decision-complete before evaluating hosting. The selected
hosting architecture is a dedicated sandboxed child with an embedded, exactly
pinned Nushell engine.

## Design Invariants

- Nushell source is the only model-facing tool-call language.
- A `<tool_call>` block is an explicit execution boundary, not data to be
  decoded as a named tool invocation.
- The complete block is given to one Nushell invocation as one program. Kraai
  does not split, schedule, approve, or continue between statements or nested
  command invocations.
- Trusted Kraai commands are installed into the Nushell execution environment
  before the raw model program is parsed. Hidden function definitions are not
  appended to the displayed model source as the primary integration boundary.
- Kraai-specific operations behave as normal Nushell commands with documented
  parameters and structured pipeline values.
- Kraai commands execute when Nushell reaches them and return native
  `PipelineData` directly to downstream stages. They are not deferred requests
  executed by the parent after the script is parsed or completed.
- Commands may produce lazy or streaming output. Kraai preserves normal
  pipeline backpressure and cancellation rather than eagerly collecting an
  entire command result at an internal boundary.
- Nushell alone parses source and binds positional arguments, flags, and pipeline
  input to Kraai command signatures. Kraai does not inspect or rewrite the
  model's script to determine how a command was invoked.
- Profiles select both the available Kraai commands and the default sandbox.
- Profiles contain an explicit sandbox permission set describing the default
  restrictions applied to scripts.
- The optional `permissions` tag attribute requests a comma-separated list of
  capability additions directly; it does not select a precomposed escalation
  mode or permission-set name.
- Requests already satisfied by the profile's effective capabilities are
  harmless no-ops. `no-sandbox` must be requested alone. The `network`
  capability covers IP networking and visible Unix-domain sockets but does not
  expose host socket paths by itself.
- Every block includes a model-authored timeout that applies to the complete
  script and its process tree. Timeout is invocation metadata, not Nushell code
  or a profile default.
- The only approval decision is whether an entire script may escalate beyond
  its default sandbox. The fallback escalation policy is an enum with
  `Deny`, `Prompt`, and `Allow` behavior. Profile permission rules may make an
  allow/deny/prompt decision before that fallback policy is consulted.
- Prompt approval is one-shot: allow once or deny once. It never mutates profile
  policy in the initial implementation.
- The active sandbox is established before any model-authored code executes and
  fails closed when its promised restrictions cannot be provided.
- User Nushell configuration, plugins, environment hooks, and startup files are
  not loaded implicitly. Execution must be reproducible from Kraai's pinned
  runtime and generated command environment.
- Profiles independently select environment inheritance, PATH behavior, and
  Nushell startup configuration. Initial profiles use an environment allow-list,
  inherit PATH with Kraai's packaged commands prepended, and start Nushell clean.
- The built-in plan profile exposes `kraai-open-files` and
  `kraai-close-files` with `workspace-read`; the built-in coding profile also
  exposes `kraai-edit-file` and grants `workspace-write`. Both use `Prompt` as
  their fallback escalation policy.
- Runtime and child-process lifetime remain controlled by cancellation and
  timeout behavior. Output limiting, truncation, and overflow artifacts are not
  part of this plan; execution returns everything Nushell produces.
- Every sandboxed script receives a private writable temporary directory without
  gaining workspace or host write capability. It is removed after the owned
  process tree exits.
- The restricted sandbox always receives the read-only runtime roots required to
  start Nushell and packaged external commands. This is a generic sandbox input,
  not a hard-coded Nix filesystem policy.
- Nix-specific store, daemon, or environment integration is optional and guarded
  by explicit configuration. The generic runner must remain usable on ordinary
  Linux without Nix behavior enabled.
- Stateful context features, especially opened-file context injection, remain
  first-class. The redesign must improve their command ergonomics without
  reducing their durability or restart behavior.
- `kraai-open-files` pins files for fresh injection into future model turns; it
  does not return their contents to the current Nushell pipeline. Immediate file
  inspection uses normal Nushell or external commands.
- Internal execution identifiers distinguish the parent script from individual
  Kraai command invocations so repeated calls and loops remain auditable.
- Every stateful Kraai command that completes successfully has its effect
  recorded durably even if a later statement fails, the script returns an
  error, or execution is cancelled. Whole-script execution is not a transaction
  over completed side effects.
- Kraai does not compact, summarize, or silently truncate script output to save
  tokens. The model owns result filtering through Nushell pipelines.
- Each accepted script produces one provider-neutral `<tool_call_result>` block
  with stable status metadata and nonempty stdout/stderr sections. Kraai keeps a
  distinct internal result role even when a provider adapter must send the
  block as ordinary input text.
- The response may contain only one `<tool_call>` block. Once its closing tag is
  observed, Kraai stops the provider stream immediately and discards any bytes
  received after the closing tag.
- Ordinary assistant text may precede the opening tag. It is streamed to the
  user and persisted as part of the assistant response so the model can explain
  its ongoing work. Requiring the block to occupy the entire response would
  prevent useful progress commentary and would be brittle to enforce.
- Kraai command signatures, short argument help, and full invocation examples
  are generated at compile time from one command declaration. Examples include
  the surrounding `<tool_call>` protocol so they teach both Nushell usage and
  invocation framing, including the required timeout.
- The final code should have one owner for sandboxing, one owner for Nushell
  execution, one owner for the Kraai command contract, and one owner for agent
  turn orchestration.
- No old DTO, parser, approval, rendering, or schema code remains merely to
  support the removed TOON protocol.
- TOON-era persisted sessions are unsupported. The redesign includes no legacy
  loader, inert-history path, or migration.
- `list-files`, `read-file`, and `search-files` are not recreated as Kraai
  commands. Nushell plus arbitrary packaged executables replace them. Ripgrep is
  available as `rg`, not wrapped in a Kraai-specific search abstraction.

## Major Workstreams

1. **Protocol and shared types**
   Replace named tool calls and JSON arguments with script execution and result
   types. Define parent execution identity, nested operation identity,
   exact result/error representation and cancellation outcomes. Add sandbox
   permission-set types, profile permission rules, and the
   `Deny`/`Prompt`/`Allow` fallback escalation policy.

2. **Sandboxed Nushell execution**
   Add a pinned, clean Nushell runtime; pass scripts without shell interpolation;
   preserve fail-closed filesystem and network restrictions; return complete
   stdout, stderr, Nushell output, and control events.

3. **Kraai command platform**
   Replace `TypedTool`, `ToolManager`, generated TOON schemas, and per-tool JSON
   DTOs with an ergonomic command registry and Nushell command definitions for
   the capabilities that remain useful, initially opened-file context state and
   safe editing. Preserve reusable filesystem logic and stateful command
   effects. Introduce a compile-time declaration mechanism that generates the
   Nushell signature, prompt help, examples, and runtime registration metadata
   without reintroducing a serialization schema language.

4. **Agent and runtime lifecycle**
   Replace parse/prepare/per-tool-approve/tool-batch orchestration with one
   whole-script execution lifecycle, optional pre-execution escalation approval,
   durable result persistence, cancellation, recovery, and exactly one
   continuation decision. Stop the provider stream at the first complete block
   and preserve successful state effects across later script failure.

5. **Profiles and prompting**
   Replace profile tool lists and numeric risk thresholds with Kraai command
   availability, an explicit sandbox permission set, pre-policy permission
   rules, and the escalation-policy enum. Generate concise Nushell command
   documentation and full protocol examples in the turn prompt.

6. **Persistence and context state**
   Persist script source, execution outcome, nested operation metadata where
   useful, and state deltas without exposing the internal wire format to the
   model. Keep opened-file snapshots reconstructible across restarts.

7. **TUI**
   Render Nushell source and results, expose running/cancelled/failed states, and
   replace the current per-tool approval queue with whole-script escalation
   approval.

8. **Packaging, evaluation, and deletion**
   Make NixOS the reference platform, pin Nushell through Nix, package every
   required helper, add token and reliability comparisons, remove
   `kraai-tool-bash`, remove TOON dependencies, delete `kraai-toon-schema`,
   delete the obsolete list/read/search tool crates, package `rg` and the
   intended general command environment, regenerate `Cargo.nix`, and run the
   complete repository gate. General Linux follows NixOS; macOS and Windows are
   outside the initial implementation.

## Success Criteria To Quantify In The Final Plan

- Models author only Nushell inside `<tool_call>` blocks.
- Multiple filesystem or process operations can be composed within one script.
- The default execution surface behaves like a clean Nushell installation with
  arbitrary commands from Kraai's intentionally packaged PATH plus the extra
  commands selected by the profile.
- Kraai commands return structured data usable by subsequent pipeline stages.
- A Kraai command can stream structured values into filtering, transformation,
  and aggregation stages during the same script execution.
- `kraai-open-files` remains a context-state command rather than an immediate
  read command, and never exposes opened file contents in its pipeline result.
- Large results can be reduced inside Nushell before entering chat history.
- Partial downstream consumption and cancellation stop unnecessary upstream
  work for streaming Kraai commands.
- Opened-file state survives continuation and process restart exactly as it does
  today.
- Sandbox setup remains fail-closed for restricted execution.
- An escalated script never begins before the profile policy permits escalation
  and the selected `Deny`/`Prompt`/`Allow` outcome is applied to the complete
  source that will run.
- When a stateful Kraai command completes and a later statement fails, its state
  effect remains visible after continuation and restart.
- Script output reaches history without token-saving summarization, compaction,
  or silent truncation.
- Every accepted script has exactly one persisted `<tool_call_result>` block;
  output that resembles result framing remains inert untrusted text.
- Command help and examples are compile-time generated from the command
  declaration, and examples demonstrate complete `<tool_call>` blocks.
- Cancellation terminates the complete process tree and produces a durable,
  understandable result.
- Tool prompt tokens, invocation tokens, returned-result tokens, round trips,
  syntax failures, task success, and execution latency are measured against the
  current TOON implementation before it is removed.
- The final workspace contains no runtime dependency on TOON and no model-facing
  JSON tool protocol.

## Plan Documents

- [01-current-system-and-removal-map.md](01-current-system-and-removal-map.md):
  the current architecture and the code that will be gutted or reshaped.
- [02-open-decisions.md](02-open-decisions.md): decisions that require product
  input before the crate and implementation plan can be completed.
- [03-target-crate-architecture.md](03-target-crate-architecture.md): proposed
  crate ownership, dependency direction, module splits, and deletion targets.
- [04-nushell-hosting-decision.md](04-nushell-hosting-decision.md): concrete
  hosting models, selected architecture, production acceptance gate, and future
  devshell seam.
- [05-production-migration-phases.md](05-production-migration-phases.md):
  production-shaped implementation order, acceptance gates, coordinated
  cutover, deletion, packaging, and final evaluation.
- [06-test-and-evaluation-matrix.md](06-test-and-evaluation-matrix.md): layered
  protocol, policy, sandbox, host, IPC, persistence, provider, Nix, and model
  evaluation acceptance cases.
- [07-final-cutover-checklist.md](07-final-cutover-checklist.md): the mechanical
  workspace migration, deletion audit, packaging, verification, and no-fallback
  completion checklist.
