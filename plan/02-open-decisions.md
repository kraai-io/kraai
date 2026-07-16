# Decisions And Open Questions

> Reading the draft did not constitute blanket sign-off. This document records
> the user's later notes as constraints while keeping every unanswered or
> partially answered decision explicit.

## Confirmed Direction

- A `<tool_call>` block contains one complete Nushell program.
- The program is handed to Nushell whole. Kraai does not split it into commands,
  schedule individual statements, or continue the model between statements.
- Scripts have the normal Nushell language and arbitrary external commands from
  Kraai's packaged PATH, plus profile-selected Kraai commands.
- Profiles control both their Kraai command surface and sandbox behavior.
- Per-tool risk assessment is removed.
- The fallback escalation policy has three outcomes: always deny, always prompt,
  or always allow.
- Profiles also contain permission rules that apply before the fallback policy
  and an explicit sandbox permission set describing normal execution.
- Permission rules match requested sandbox capabilities, not Nushell source or
  command prefixes. A read-only plan profile can therefore allow, prompt, or
  deny a request for workspace write access needed by commands such as
  `cargo test`.
- The `permissions` attribute requests capabilities directly. It does not name
  a precomposed permission set.
- Workspace-only read access and broad host read access are distinct
  capabilities.
- A prompted request offers allow once or deny once. Persistent approval choices
  are deferred future work.
- Every script has a timeout specified by the model because appropriate runtime
  varies by invocation. Timeout is not selected only by the profile.
- Scripts always start at the workspace root in the initial implementation;
  profile-configurable starting directories are deferred.
- Escalation applies to the entire script and is approved before any part runs.
  There is no mid-script approval.
- `list-files`, `read-file`, and `search-files` are deleted. Normal Nushell
  commands replace list/read behavior, and packaged `rg` replaces specialized
  search.
- `kraai-tool-bash` is deleted.
- `kraai-toon-schema` is deleted from Kraai without a repository-extraction
  task; it can be recovered from commit history.
- A response contains at most one `<tool_call>` block. Kraai stops the provider
  stream as soon as the closing tag is complete and discards anything after it,
  including a model-generated newline or prose prompting for the result.
- Ordinary assistant text is allowed before the opening tag. It is streamed and
  persisted normally so the model can tell the user what it is doing before the
  script begins.
- Whole-script execution is not transactional rollback. Every completed effect
  remains a completed effect even if a later statement fails.
- Script output is not compacted, summarized, or silently truncated for token
  reduction.
- Output limits and overflow artifacts are explicitly outside this redesign;
  Kraai returns everything the script produces.
- Kraai commands retain concise schemas/help and examples, generated at compile
  time. Examples show complete `<tool_call>` invocations.

## 1. Nushell Hosting And Kraai Command Bridge

This is now evaluated in detail in
[04-nushell-hosting-decision.md](04-nushell-hosting-decision.md). The current
selected design is a dedicated sandboxed child with an embedded, exactly pinned
Nushell engine. External definitions/helpers and plugins are rejected.

The initial proposal was to concatenate the model-authored source with trusted
Nushell function definitions and run the combined script. Nushell parses command
definitions for the whole script, so definitions can technically appear after
their call sites. The plan should still avoid making concatenated trusted source
the architectural boundary:

- Model source should be stored, approved, diagnosed, and executed byte-for-byte
  rather than rewritten with hidden text.
- Trusted native command registration happens in the fresh engine state before
  parsing the model program.
- The model must not be able to shadow, replace, or directly invoke a private
  helper transport and thereby forge state deltas or operation events.
- Parser spans and errors should refer directly to the source shown in the TUI,
  without a generated prelude changing line offsets.

### A. External Nushell plus generated `def` wrappers

Kraai launches pinned `nu`. Generated typed functions call a dedicated Kraai
helper executable inside the same sandbox. The helper returns a private
structured transport value that the wrapper converts into a native Nu value.

- Lowest coupling to Nushell's Rust internals.
- Easy to inspect and reproduce generated scripts.
- A helper process per command may be expensive unless a persistent bridge is
  introduced.
- State deltas and operation events need a separate inherited control channel.

### B. Dedicated child runner with an embedded Nushell engine

Kraai launches a sandboxed runner process that embeds Nushell and registers
Kraai commands directly as Rust commands.

- Direct access to native Nu values and command signatures.
- Avoids spawning a helper for every command.
- Couples Kraai to a large and fast-moving Nushell crate surface.
- Must remain a child process so model-authored code executes inside the OS
  sandbox rather than the parent agent process.
- Kraai commands run with trusted in-process context inaccessible to Nushell
  source and forward each completed state effect immediately over an
  authenticated, sequenced parent channel. The parent durably acknowledges the
  effect before the command returns success. Close-on-exec descriptors and
  sandboxed process-inspection controls reduce channel exposure to external
  descendants; effects are not delayed until final evaluation.
- Can construct the trusted command environment first and parse the exact model
  source afterward, without concatenating definitions into it.

### C. External Nushell plus a pinned Kraai Nushell plugin

Kraai launches pinned Nushell 0.114.0 with a version-matched plugin exposing the
Kraai commands.

- Uses Nushell's intended structured extension protocol.
- Gives commands native signatures and pipeline values while keeping shell and
  command implementations in separate processes.
- `nu-plugin` and `nu-protocol` must be pinned to the exact compatible Nushell
  release.
- The official Rust plugin defaults may use a local socket. Kraai's restricted
  network seccomp blocks socket connection syscalls, so this rejected design
  would have required another transport/sandbox compatibility constraint.
- Plugin registry generation, startup cost, cancellation, and state-event
  transport also require measurement.

The earlier non-hosting contracts are resolved. The full comparison and the
production acceptance gate are retained in the dedicated document as rationale
and validation criteria for the selected embedded-child design.

Future per-script Nix devshell and direnv support is outside the initial
implementation, but the selected topology must support a configurable
environment-provider stage that wraps only the sandboxed child execution. Kraai
itself must not need to start inside the devshell.

## 2. Escalation Policy And Request Syntax

Three concepts should not be conflated:

1. A profile sandbox permission set describes what normal sandboxed execution
   may read, write, execute, inherit, and access over the network.
2. Capability-based profile permission rules are evaluated before the fallback
   policy and can produce `Deny`, `Prompt`, or `Allow` for requested additions
   such as workspace write or network access.
3. A fallback escalation policy produces `Deny`, `Prompt`, or `Allow` when no
   earlier profile permission rule decides the request.
4. A script either uses the profile's default permission set or requests one or
   more additional capabilities for that invocation.

`Deny` rejects the escalated script without running it. `Prompt` shows the whole
script to the user and runs it only after approval. `Allow` runs the whole script
escalated without prompting.

Rules do not match raw Nushell source or command prefixes. Source text is not
equivalent to a single argv command and cannot reliably predict a program's
effects.

The initial capability vocabulary and aggregation behavior are resolved below.

### Resolution model

1. The profile supplies the default `SandboxPermissionSet`.
2. The tag requests zero or more capabilities to add for this script, for
   example `permissions="workspace-write"` or
   `permissions="workspace-write,network"`.
3. Reject `no-sandbox` if it appears with any other requested capability.
4. Compute semantic capability closure and remove additions already granted by
   the profile. For example, `host-read` already includes `workspace-read`. If
   no additions remain, run with the default sandbox without prompting.
5. The prompt documents the capability names understood by the active profile.
6. A matching profile permission rule decides each remaining capability first.
7. The fallback escalation policy decides any capability without a matching
   rule.
8. If any requested capability resolves to `Deny`, reject the whole script. If
   none are denied but any resolve to `Prompt`, show one combined prompt. Run
   automatically only when every requested capability resolves to `Allow`.
9. If allowed or approved, the complete effective permission set is established
   before Nushell starts.

This lets a plan profile default to read-only execution while still allowing a
script such as `cargo test` to request `workspace-write`. Approval broadens only
that script; it does not silently change the profile default.

Proposed initial capability vocabulary:

- `workspace-read`: permit reads under the workspace and configured
  workspace-adjacent roots.
- `host-read`: permit reads anywhere the Kraai process's operating-system
  credentials allow. It subsumes `workspace-read` but does not change write,
  network, or other isolation.
- `workspace-write`: add writes under the workspace while retaining protected
  metadata restrictions.
- `network`: permit IP networking and communication with Unix-domain sockets
  visible inside the sandbox. It does not mount or otherwise expose host sockets
  by itself; optional integrations such as the Nix daemon control their own
  visibility.
- `metadata-write`: permit writes to protected workspace metadata such as
  `.git`, `.jj`, `.kraai`, `.agents`, and `.codex`.
- `host-write`: permit writes anywhere the Kraai process's operating-system
  credentials allow. It subsumes `workspace-write` and `metadata-write`, but it
  does not independently enable network access or remove other isolation.
- `no-sandbox`: run the script without Kraai's filesystem, network, and syscall
  sandbox. Process-tree ownership, the model-authored timeout, selected
  environment policy, and clean-startup policy still apply. It must be requested
  alone because every other sandbox capability is redundant with it.

The initial design does not add capabilities for configured sets of extra read
or write roots. Narrower filesystem scopes can be introduced later if the
workspace/host split proves too coarse.

### Baseline runtime roots

Capabilities describe model-visible filesystem scope, but a restricted process
also needs enough read-only runtime state to start. Every sandbox therefore
receives an explicit set of read-only runtime roots for Nushell, dynamic
libraries, packaged executables, certificates, and other required runtime data.
Those roots do not imply `host-read` and must not expose unrelated user data.

The sandbox contract is platform-neutral: its input is a resolved list of
runtime roots, not assumptions about `/nix/store`, FHS paths, or a particular
package manager. Platform/package integration owns construction of that list.
Any Nix-specific store discovery, daemon access, mounts, or environment behavior
is guarded by an explicit configuration flag. NixOS may enable that integration
in its packaged profile, but the generic Linux runner must work with it disabled.

### Prompt behavior

- Approval offers allow once or deny once.
- Approval does not mutate the profile, create a session rule, or create a
  persistent rule. Persistent approval UX is deferred future work.
- Denying an escalation returns a normal denial result and automatically
  continues the model so it can attempt a sandboxed alternative.

The prompt displays all three relevant inputs to the decision:

- The complete raw script that will run unchanged.
- The capabilities requested by the model.
- The effective capability difference from the profile's default sandbox.

### Recommended model-facing syntax

Keep a single tag and put Kraai execution metadata in an optional XML attribute:

```nu
<tool_call timeout="30sec">
kraai-open-files Cargo.toml
</tool_call>
```

```nu
<tool_call permissions="workspace-write" timeout="10min">
cargo test
</tool_call>
```

Multiple capabilities use a comma-separated list:

```nu
<tool_call permissions="workspace-write,network" timeout="10min">
cargo test
</tool_call>
```

Reasons:

- Nushell source stays pure and can be passed to Nushell unchanged.
- A second executable tag would duplicate stream/parser/TUI paths.
- An argument inside the script would require Kraai to interpret Nushell source
  or reserve a fake command before it knows how the script must be sandboxed.
- An extensible attribute expresses the requested capability additions without
  changing the script language.
- The default form remains the shortest possible invocation.

The parser trims whitespace around comma-separated capabilities, rejects empty
or duplicate entries, and rejects unknown attributes or capability names rather
than silently weakening or strengthening the sandbox. A timeout is required on
every block, uses the pinned Nushell duration spelling such as `30sec` or
`10min`, and is validated before policy evaluation or execution. Kraai does not
supply a default or impose a policy maximum; either can be added later if
needed.

### Why retain an explicit tag

The provider emits ordinary assistant text, so Kraai needs an unambiguous marker
that distinguishes executable source from explanations and examples. Markdown
code fences are more likely to appear as non-executable prose, while a different
sentinel still has the same delimiter-collision problem. No currently identified
alternative materially improves token use or parsing enough to replace
`<tool_call>`.

The final parser must be a streaming state machine that understands the chosen
start tag and attributes. It must not continue using a regex as the protocol
authority.

The parser emits leading assistant text incrementally until it recognizes a
valid opening tag. It then switches to script capture. The opening tag, script,
and closing tag remain part of the persisted assistant response, while bytes
after the first complete closing tag are discarded even if they arrived in the
same provider chunk.

## 3. Command Surface And Profiles

Resolved:

- The default experience is a clean Nushell installation with arbitrary
  commands from Kraai's intentional runtime PATH.
- The user Nushell configuration, autoload directories, hooks, history, and
  personal plugin registry are not loaded.
- Kraai adds only capabilities that remain useful beyond normal shell commands.
- Profiles select both their Kraai command set and their default sandbox.
- `kraai-open-files`, `kraai-close-files`, and `kraai-edit-file` remain planned
  commands unless later discussion removes or redesigns them. The `kraai-`
  prefix establishes a collision-free initial namespace while preserving normal
  Nushell commands such as `open`.
- `kraai-open-files` pins validated paths for fresh injection into future model
  turns. It never returns file contents to the current script; its immediate
  result is limited to operation status and normalized path metadata. Immediate
  reads use normal Nushell `open` or external commands such as `cat`.
- `rg` is packaged as an ordinary external command, not implemented as a Kraai
  command.

The exact environment allow-list and inherited PATH construction remain
implementation-plan decisions.

- Inheriting the launching process's PATH and environment best reproduces the
  user's real failures and makes Kraai behave like their normal Nushell.
- A deterministic packaged PATH and minimal environment are more reproducible
  and reduce accidental secret exposure, but may hide the exact configuration
  problem the model is supposed to diagnose.
- The sandbox constrains filesystem and network effects, but it does not make
  inherited environment variables confidential from model-authored code.

The sandbox permission-set design should therefore include an explicit
environment policy rather than letting inheritance be an undocumented process
spawn side effect.

Profile dimensions:

```text
environment: minimal | inherit | allow-list
nushell_startup: clean | inherit
path: inherit | packaged
```

These dimensions are independent: a profile may inherit the process environment
and PATH for faithful diagnosis while still starting Nushell without the user's
config, autoload scripts, hooks, history, or plugin registry.

Reasonable initial defaults for both the plan and normal coding profiles:

```text
environment: allow-list
nushell_startup: clean
path: inherit
```

Kraai's packaged command directory is prepended to the inherited PATH so pinned
baseline commands remain available. The environment allow-list contains only
the variables required for ordinary command execution and locale/terminal
behavior; inheriting the complete environment remains an explicit profile
choice because it can expose credentials to model-authored scripts. The exact
allow-list is implementation-plan work and should have one shared definition,
not profile-specific copies.

### Initial built-in profiles

The first built-in profiles are fixed as follows:

```text
plan:
  commands: kraai-open-files, kraai-close-files
  capabilities: workspace-read
  escalation_policy: prompt

coding:
  commands: kraai-open-files, kraai-close-files, kraai-edit-file
  capabilities: workspace-read, workspace-write
  escalation_policy: prompt
```

Both use the common environment defaults above, configured read-only runtime
roots, and private temporary storage. The plan profile can request
`workspace-write` for scripts such as `cargo test`, but approval does not make
`kraai-edit-file` available because command availability and sandbox capability
are independent profile dimensions.

## 4. Whole-Script Execution And Atomicity

Resolved:

- One block equals one script and one parent execution ID.
- The complete source is parsed and executed by one Nushell invocation.
- Nested native, external, and Kraai commands are synchronous parts of that
  program, not agent tool-call lifecycle boundaries.
- There is no model continuation during the script.
- The runtime persists one final outcome and makes at most one continuation
  decision.
- An escalated script is approved or denied as a whole before it starts.
- Each stateful Kraai command effect is recorded when that command completes and
  survives a later script error or cancellation.
- The provider stream is stopped at the first complete closing tag; a second
  block is never accepted from the same response.

“Almost atomic” is currently interpreted as one scheduling, approval,
cancellation, persistence, and continuation unit—not transactional rollback of
filesystem or process side effects. Nushell statements naturally take effect as
the program executes, and arbitrary external side effects cannot be rolled back.

This requires a reliable state-event path from the runner to the parent while
the script is still executing. Waiting until process exit would lose completed
state effects on timeout or forced cancellation. Whichever hosting mechanism is
chosen later must prevent model-authored Nushell from forging these state
events.

## 5. Result Fidelity

Resolved:

- Kraai does not compact, summarize, filter, or silently truncate script output.
- Information reduction is authored explicitly by the model in the Nushell
  program, not imposed after execution.
- Kraai returns everything the script produces.
- Output limiting, truncation, compaction, summarization, and overflow-artifact
  behavior are deferred future work and must not be introduced by this plan.

The result envelope below is the only output-representation design in scope.
The implementation must preserve complete output; a later feature can define
limits or alternate storage if that becomes necessary.

### Provider-facing result envelope

Resolved initial contract:

```xml
<tool_call_result status="completed" exit_code="0">
<stdout>
...exact stdout and final Nushell rendering...
</stdout>
<stderr>
...exact stderr and diagnostics...
</stderr>
</tool_call_result>
```

- Kraai produces exactly one `<tool_call_result>` block for each accepted
  `<tool_call>` script execution.
- The required `status` attribute uses the stable script status vocabulary.
  `exit_code` is present only when a process exit code exists.
- `stdout` and `stderr` sections are included only when nonempty. A result with
  no textual output may use an empty body.
- Section contents are preserved exactly. Kraai does not escape, summarize,
  compact, or reinterpret output merely because it resembles XML or contains a
  closing tag.
- The tag is presentation framing for the model, not executable protocol input.
  `kraai-script-protocol` never parses result blocks, and output text cannot
  create another execution.
- Internally, Kraai persists structured execution metadata and output channels
  separately from the rendered block so the presentation can be revisited
  without losing information.
- A provider-neutral `ToolCallResult` history role distinguishes the block from
  a human-authored user message inside Kraai. Provider adapters map that role to
  ordinary input text when the provider has no matching native role; they do not
  synthesize provider-native tool calls or add another `[Tool Result]` prefix.
- The system prompt explains that result contents are untrusted program output,
  not higher-priority instructions.

This preserves Kraai's existing fully custom execution and continuation model.
Only result granularity and framing change: the old path emits one formatted
message per named tool, while the new path emits one result for the whole
Nushell script.

## 6. Compile-Time Command Documentation

Resolved:

- Every Kraai command has a concise schema/help description of its positional
  arguments, flags, pipeline input, and output.
- Kraai commands execute at their normal position in Nushell evaluation and
  return native structured pipeline values directly to downstream stages.
- Lazy or streaming command output remains lazy or streaming across the command
  boundary; it is not eagerly collected or routed through the Kraai parent.
- Every Kraai command has examples.
- Both are generated at compile time from the command declaration.
- Prompt examples include complete `<tool_call>` blocks so they demonstrate the
  invocation protocol as well as the command itself.
- External commands such as `rg`, Git, Cargo, and Jujutsu do not receive Kraai
  schemas; they remain ordinary commands in the Nushell environment.

The selected starting design is a declarative macro in `kraai-command-core`
that generates native Nushell registration and static prompt metadata from one
declaration. A proc-macro crate is added only if implementation proves the
declarative form cannot express the required signatures and examples cleanly;
it is not part of the initial architecture by default.

## 7. Platform Scope

Resolved priority order:

1. NixOS is the reference and first supported platform.
2. General Linux support is second priority.
3. macOS and Windows support are deferred until there is a practical way to
   develop and test their sandbox implementations.

The Nushell execution, command, and lifecycle interfaces should remain portable.
Platform-specific sandboxing stays behind a narrow boundary because it is
expected to be the main macOS/Windows implementation difference. This plan does
not include macOS or Windows sandbox design, implementation, packaging, or
support claims. NixOS-specific conveniences are configuration-controlled layers
above the generic sandbox contract rather than requirements embedded in it.

## 8. Existing Session Data

Resolved: existing TOON-era sessions are unsupported. Kraai retains no legacy
parser, schema, DTO, specialized renderer, fallback loader, inert-history mode,
or migration. Users start new sessions after the redesign. The only known old
data belongs to the developer, and preserving it is not a product requirement.

## 9. Script Process Contract

Resolved for the initial implementation:

- A script starts in the workspace root and may use normal Nushell `cd` semantics
  during that invocation. Profiles cannot override the starting directory in
  the initial redesign.
- Scripts are noninteractive. They receive no TTY and no model-provided stdin;
  commands that attempt to read stdin observe EOF rather than hanging forever.
- One execution owns the complete process tree. Cancellation or timeout kills
  the entire tree.
- Background children cannot survive script completion, failure, cancellation,
  or timeout.
- Every invocation includes a model-authored timeout for the complete script and
  process tree. There is no model-free profile timeout substituted for it.
- The execution timeout starts immediately before Nushell launches. Time spent
  waiting for user approval does not consume the requested execution duration.
- Every sandboxed invocation receives a fresh private writable temporary
  directory and the corresponding temporary-directory environment variables.
  This does not grant workspace or host write capability. Cleanup happens only
  after the complete owned process tree has exited or been killed.
- A denied capability request returns a normal denial result and continues the
  model once without running any part of the script.

The workspace root remains the only persistent working tree by default; private
temporary files are execution-local scratch space.

## 10. Script Execution Status

Resolved stable statuses:

- `completed`
- `denied`
- `invalid-script`
- `timed-out`
- `cancelled`
- `sandbox-unavailable`
- `failed-to-start`

The applicable status is persisted durably and returned to the model together
with Nushell diagnostics and all output produced before termination. The final
crate plan must define the Rust enum and persisted representation from this one
status vocabulary rather than duplicating status models across the runner,
runtime, persistence layer, and TUI.

`completed`, `denied`, `invalid-script`, `timed-out`, `sandbox-unavailable`, and
`failed-to-start` return their result and continue the model exactly once. A
user-initiated `cancelled` execution stops without automatic continuation.
