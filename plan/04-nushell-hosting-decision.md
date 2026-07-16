# Nushell Hosting Decision

> Status: selected architecture. This document records why Kraai will use a
> dedicated child with an embedded Nushell engine, rejects the definition/helper
> and plugin designs, and preserves an extension point for future per-script
> devshells.

## The Decision In One Sentence

Kraai should own a dedicated `kraai-nushell-host` child executable, launch it
inside `kraai-sandbox`, embed the exactly pinned Nushell engine in that child,
and register profile-selected Kraai commands as native Rust Nushell commands
before parsing the exact model source.

This is not embedding untrusted Nushell into the main Kraai process. The child
process is the security and lifetime boundary.

## Common Process Topology

All viable options need per-script child execution:

```text
Kraai parent
  -> resolve profile, capabilities, approval, and timeout
  -> prepare per-script execution environment
  -> establish OS sandbox and process-tree ownership
  -> start Nushell execution child
  -> stream output and trusted state events
  -> persist final status and continue once
```

Keeping this topology independent from the hosting choice enables a future
devshell provider without requiring Kraai itself to run in that devshell.

## Option A: External Nushell With Generated Definitions

### How it works

Kraai packages the normal `nu` executable. Before execution it generates a
trusted startup module containing definitions such as:

```nu
export def kraai-open-files [...paths] {
  # Encode native input, invoke a private helper, decode its result.
}
```

Kraai then starts clean Nushell with the trusted module/config loaded before a
separate file containing the exact model script. The definitions invoke helper
executables that implement the stateful operations.

The model does not author the helper serialization, so this would still satisfy
the model-facing no-JSON/no-TOON goal. The serialization merely moves behind the
boundary.

### Advantages

- Uses the official Nushell CLI behavior directly.
- Requires little coupling to Nushell's Rust engine crates.
- Trusted definitions can be inspected independently from model source.
- A pinned absolute `nu` path is easy to launch through a devshell wrapper.

### Costs and risks

- A Nushell `def` is a wrapper, not a native host capability. Structured values
  must cross a helper protocol for every Kraai command call.
- A helper process per call adds startup cost; a persistent helper recreates an
  RPC/plugin system.
- Completed state effects require a trusted side channel while the script is
  still running. Waiting for final stdout loses effects on timeout.
- Any helper transport reachable from arbitrary model-authored external
  commands must authenticate commands, capabilities, invocation IDs, and state
  effects. The apparent simplicity moves complexity into a private protocol.
- Generated definitions, helper signatures, command documentation, and runtime
  registration can drift unless another source-of-truth layer is introduced.
- The model can inspect or shadow ordinary definitions unless the startup and
  scope rules are constrained carefully.

### Assessment

Reject as the primary architecture. It is useful for a prototype, but it is the
weakest long-term fit for stateful commands, native structured data, and a
non-forgeable event path.

## Option B: Dedicated Child With Embedded Nushell

### How it works

`kraai-nushell-runtime` builds a `kraai-nushell-host` executable linked against
an exactly pinned set of Nushell crates. The child:

1. Creates a clean Nushell `EngineState`.
2. Adds the normal language and command sets intended for the Kraai runtime.
3. Registers only the active profile's Kraai commands as native Rust commands.
4. Parses the exact raw model source after registration.
5. Executes the entire program once and returns native values/output.
6. Emits completed state effects through trusted in-process command context as
   soon as each stateful command finishes.

Nushell's public `Command` trait supplies the native command name, signature,
description, examples, pipeline input, and `run` implementation. The same
compile-time Kraai declaration can generate the static prompt metadata and the
native command implementation.

The child receives the script and control data through private descriptors, not
argv interpolation. The request descriptor is consumed and closed before model
source is evaluated. The trusted event path uses close-on-exec descriptors plus
per-execution authenticated, sequenced frames; descriptor secrecy alone is not
treated as an authorization boundary.

### Advantages

- Kraai commands use native Nushell `Value`/`PipelineData` without a helper or
  plugin serialization hop.
- Command registration is an engine capability, not model-visible wrapper text.
- The model cannot forge Rust command context, command IDs, or state events.
- Trusted commands are installed before the unmodified model source is parsed,
  preserving diagnostic line numbers.
- Only one dedicated host process is required before model-launched external
  commands.
- Active profile command selection is direct: unregistered commands do not
  exist in the engine.
- The child can be wrapped by `nix develop --command`, `direnv exec`, or another
  future environment provider without moving the Kraai parent.

### Costs and risks

- Kraai depends on Nushell engine implementation crates, whose APIs evolve with
  Nushell.
- Kraai must deliberately assemble a command set equivalent to the promised
  clean Nushell experience; accidentally omitting commands would violate the
  product contract.
- CLI behavior for environment conversion, diagnostics, stdlib loading,
  external commands, and signals must be reproduced or reused correctly.
- Builds and the runtime closure will be larger, and Nushell upgrades require a
  coordinated compatibility pass.

### Required mitigations

- Pin all Nushell crates to one exact full version and upgrade them together.
- Add conformance tests comparing the host against the pinned clean `nu` binary
  for representative language, pipeline, external-command, environment, error,
  and output behavior.
- Keep all Nushell-specific imports inside `kraai-nushell-runtime` and
  `kraai-command-core`; the rest of Kraai depends only on stable local contracts.
- Keep the host a separate sandboxed executable even if an in-process library
  call looks easier during implementation.
- Build the normal command set from the same Nushell crate versions used by the
  pinned CLI package.

### Assessment

Selected. Kraai owns the whole execution appliance, so a native embedded engine
inside a disposable sandboxed child is a better boundary than extending a
separately owned interactive shell.

## Selected Design In Detail

### Process ownership

```text
long-running Kraai parent
  -> optional future environment-provider wrapper
  -> kraai-sandbox launch plan
  -> fresh kraai-nushell-host child
       -> one fresh Nushell EngineState
       -> profile-selected native Kraai commands
       -> model-launched external descendants
```

The parent owns policy, approval, timeout, persistence, provider continuation,
and final status. The child owns engine initialization and exactly one script.
The child never accepts a second script, and a host process is not pooled or
reused across turns in the initial design.

Even `no-sandbox` uses the dedicated child. It removes OS isolation but does not
move Nushell into the long-running parent or weaken process-tree ownership,
timeout, environment, startup, and event-authentication behavior.

### Launch contract

After approval, the parent creates one immutable internal request containing:

- Script execution ID.
- Exact raw Nushell bytes.
- Required timeout.
- Fixed workspace-root working directory.
- Effective capability closure.
- Active command IDs.
- Resolved environment, PATH, startup policy, and runtime roots.
- Private temporary-directory information.
- A fresh per-execution event-authentication secret.

This internal structure may use an efficient private serialization format. It is
not model-authored, never enters the prompt, and is not the command API.

The parent passes the request over a dedicated pipe or equivalent descriptor.
The host reads it once, validates it, retains the authentication secret only in
Rust memory, and closes the request descriptor before parsing model source. The
script is never placed in a shell command string or interpolated into argv.

Separate channels carry:

- Normal Nushell/stdout output.
- Nushell/external stderr and diagnostics.
- Authenticated control, invocation, and state-effect frames.
- Parent acknowledgments for durable state effects.

Scripts receive EOF as stdin. Internal channels never reuse stdin, stdout, or
stderr in a way visible as ordinary script data.

### Fresh engine construction

For each script the host:

1. Creates the base language `EngineState` from the pinned `nu-cmd-lang` stack.
2. Adds the normal shell command context from the same pinned Nushell release.
3. Loads the normal default standard-library/prelude behavior promised by a
   clean Kraai Nushell installation.
4. Applies the resolved environment, PATH, workspace-root current directory,
   private temp directory, and noninteractive settings.
5. Does not load user `env.nu`, `config.nu`, autoload directories, history, or
   personal plugin registry unless a future profile feature explicitly says so.
6. Registers only the active profile's Kraai commands.
7. Parses the exact model bytes with a synthetic source filename but no prepended
   definitions or offset-changing wrapper text.
8. If parsing/compilation succeeds, evaluates the complete block once.
9. Uses Nushell's normal noninteractive rendering for the final pipeline value
   and waits for all owned descendants to terminate.

The implementation should reuse Nushell's own initialization helpers where they
fit rather than manually reconstructing defaults. Because those helpers are
internal versioned APIs, conformance tests—not assumed API stability—define
correct behavior.

### Native Kraai command adapter

Each individual command crate supplies one command implementation through
`kraai-command-core`. A declarative compile-time command declaration generates:

- Static prompt name, description, signature help, and complete examples.
- The matching native Nushell `Command` name, `Signature`, examples, and
  adapter.
- Required capability metadata.
- Runtime registration metadata.

Nushell parses and type-checks positional arguments, flags, and pipeline input.
The adapter receives the native bound call and `PipelineData`; it does not parse
source or decode a model-authored DTO.

Command invocation is synchronous with Nushell evaluation, not a request queued
for the Kraai parent. When Nushell reaches a Kraai command in a pipeline, its
native Rust implementation runs immediately inside the host and returns
`PipelineData` directly to the next Nushell stage. Intermediate structured
values stay inside the engine; they are not rendered to text, sent through the
parent, and parsed back into Nushell.

A command may return a single value or a lazy/streaming pipeline according to
its declared contract. Downstream stages can filter, transform, aggregate, or
consume those values with normal Nushell behavior. For example, a hypothetical
future search command could support:

```nu
kraai-search-web "Nushell engine embedding"
| where score > 0.8
| first 5
```

`kraai-search-web` is illustrative, not part of the initial command set.
`kraai-open-files` intentionally has a different contract: it validates and
pins paths for fresh context injection on future model turns. It never returns
file contents to the running script. Its immediate result contains only
lightweight operation status and normalized path metadata. A model that needs
contents during the current script uses ordinary Nushell commands such as
`open` or an external command such as `cat`.

The runtime must preserve Nushell's pipeline backpressure and cancellation
behavior. It must not eagerly collect a command's complete stream merely to
cross a Kraai abstraction boundary. A future long-running command can therefore
produce records incrementally while the same script consumes them.

The trusted command context contains:

- Effective capability closure.
- Workspace and explicitly scoped filesystem access.
- Script and nested command invocation IDs.
- The authenticated state-effect client.

Registration enforces profile command availability. Every command also checks
its required effective capabilities at execution as defense in depth, so a
registry bug cannot silently grant a broader filesystem operation.

### Durable state-effect handshake

Stateful commands cannot wait until final process output to report effects: a
later timeout would lose already completed open/close state. They use a durable
handshake instead:

```text
native command completes domain operation
  -> allocate trusted command invocation ID and sequence number
  -> send authenticated state-effect frame
  -> parent validates execution ID, sequence, and authentication
  -> parent persists effect atomically
  -> parent sends acknowledgment
  -> native command returns success to Nushell
```

Therefore, a stateful Kraai command is not considered successfully completed
until the parent has durably acknowledged its effect. If persistence or the
control channel fails, the command returns an error. Any underlying filesystem
effect that occurred before that failure is reported honestly and is not
pretended to have rolled back.

This handshake is orthogonal to command output. Stateless commands never use it.
For a future streaming stateful command, each successfully completed item sends
and receives any required durable effect acknowledgment before that item's
success value becomes visible downstream; later items may still fail. The
pipeline does not wait for the whole command or whole script before consuming
earlier items. This is a general command-platform rule, not the output contract
for `kraai-open-files`.

The event writer lives in trusted Rust code and holds the per-execution secret
outside Nushell values and environment variables. Frames are authenticated and
sequenced so access to a process descriptor through `/proc` is not enough to
forge or replay a state effect. Descriptors are also marked close-on-exec before
model-launched external processes begin. The sandbox must independently deny
process-memory inspection and tracing of the host; channel authentication is
defense in depth, not a substitute for process isolation.

Output remains separate from control traffic. Text resembling an internal event
on stdout or stderr is always ordinary output.

### Sandbox relationship

`kraai-sandbox` starts the host after capabilities are resolved. The host and all
native Nushell commands execute inside the same filesystem, network, and syscall
boundary. External commands launched by Nushell are descendants of that host and
cannot escape the already-established sandbox.

The sandbox supplies:

- Generic read-only runtime roots for the host, Nushell commands, and packaged
  executables.
- Workspace and host filesystem scope derived from effective capabilities.
- Protected metadata behavior under `workspace-write`.
- Private writable temporary storage.
- Network/Unix-socket behavior.
- Process-tree ownership and termination.

The host cannot ask the sandbox to broaden permissions mid-script. Requested
capabilities were approved and fixed before the host started.

### Timeout, cancellation, and final status

The requested timeout clock begins immediately before the host launches; user
approval time is excluded. The parent remains authoritative for timeout and
user cancellation because it owns the process tree.

- A parse or compile diagnostic maps to `invalid-script` without evaluation.
- Failure to launch or initialize the host maps to `failed-to-start`.
- Failure to establish promised isolation maps to `sandbox-unavailable`.
- Parent timeout kills the tree and persists `timed-out` with all captured
  output and acknowledged effects.
- User cancellation kills the tree, persists `cancelled`, and does not continue
  the model.
- Normal engine/process termination maps to `completed` with exit and Nushell
  success/error detail.

The parent does not trust a child-reported final status over direct process and
sandbox observations. Child events add diagnostics; parent lifecycle evidence
decides timeout, cancellation, startup, and sandbox failures.

### Output path

The host forwards ordinary Nushell rendering, external stdout/stderr, and
diagnostics without inserting control markers. The parent captures everything
and persists the complete result as already agreed. Model-authored Nushell
pipelines remain the only in-scope result-reduction mechanism.

After final status is known, the agent renders one `<tool_call_result>` history
message from the separately persisted status, stdout, and stderr channels.
Nonempty channels receive explicit sections; their contents remain unchanged.
This rendering occurs in the parent after execution and is never fed back into
the Nushell host or model-facing script parser.

### Performance model

The initial architecture pays for one fresh host/engine startup per script. It
avoids per-Kraai-command helper processes and a plugin process. Measure:

- Host cold-start latency.
- Engine-context construction and stdlib cost.
- Runtime closure and final binary size.
- Native command invocation overhead.
- Time to first pipeline item and streaming throughput.
- Backpressure behavior when downstream consumes only part of a stream.
- External command launch overhead.

Do not add a persistent host pool initially. Reuse would complicate per-script
sandboxing, environment providers, cleanup, cancellation, and state isolation.
If startup becomes material in evaluation, optimization must preserve the fresh
execution semantics rather than silently sharing mutable engine state.

### Nushell upgrade boundary

All Nushell crates use one exact full version and are upgraded atomically. The
upgrade checklist runs:

- Native command compile tests.
- Prompt metadata/signature consistency tests.
- Clean-CLI conformance tests.
- Structured pipeline, lazy-stream, backpressure, and early-cancellation tests.
- Sandbox/external-command tests.
- Diagnostics/output snapshots.
- Performance and closure-size comparison.

No other Kraai crate imports Nushell engine internals. This confines expected
API churn to the host and native command adapter instead of spreading it across
agent/runtime/persistence/TUI code.

## Option C: External Nushell With A Kraai Plugin

### How it works

Kraai packages the normal `nu` executable and a version-matched
`nu_plugin_kraai` executable. Nushell loads it from an explicit registry or the
CLI's plugin path option. The plugin advertises native signatures and exchanges
typed values with Nushell over the official plugin protocol.

The plugin would be a composition executable depending on the individual Kraai
command crates; command implementations would remain one crate per command.

### Advantages

- Uses the official Nushell CLI, startup, stdlib, and external-command behavior.
- Plugins expose native command signatures and structured pipeline values.
- Nushell provides compatibility negotiation and manages plugin calls.
- Command implementations remain Rust rather than generated Nushell wrappers.

### Costs and risks

- The plugin and Nushell must use matching protocol/crate versions; Nushell's
  documentation explicitly warns users to update plugins with Nushell.
- Plugin calls serialize typed values through JSON or MessagePack and require a
  second long-lived process.
- Rust plugins default to local-socket transport when that feature is enabled;
  the restricted sandbox would need a build that forces stdio or deliberately
  permits the socket path/syscalls.
- Direct CLI plugin loading is currently marked experimental by the pinned
  Nushell CLI, while registry generation adds mutable/cache lifecycle work.
- The plugin process still needs a durable state-event channel to Kraai.
  Nushell's plugin result protocol does not itself persist Kraai context effects
  immediately in the parent runtime.
- Plugin lifecycle controls allow the script to stop plugins, and garbage
  collection/relaunch behavior adds failure states that do not exist for native
  embedded commands.
- Command availability is weaker as a capability boundary because the plugin is
  a discoverable executable inside the runtime closure.

### Assessment

Rejected, including as an automatic fallback. It adds a protocol and process
without providing an advantage Kraai needs when Kraai already controls a
dedicated child host. If the selected architecture encounters a fundamental
problem, the design returns for explicit review rather than silently changing to
plugins.

## Comparison

| Criterion | Generated definitions | Embedded child | External plugin |
|---|---|---|---|
| Exact normal Nu CLI startup | Strong | Must reproduce/test | Strong |
| Native structured command values | Wrapper serialization | Direct | Plugin serialization |
| Trusted immediate state effects | Difficult | Direct | Separate channel needed |
| Profile-selected command absence | Wrapper/scope discipline | Native registration | Plugin load/registration |
| Process count | Nu plus helpers | One host | Nu plus plugin |
| Nushell version coupling | CLI only | Engine crate APIs | Exact plugin protocol |
| Clean diagnostic spans | Possible with separate source | Direct | Direct |
| Sandbox compatibility | Good | Best | Stdio must be forced/tested |
| Future devshell wrapper | Yes | Yes | Yes |
| Decision | Rejected | Selected | Rejected |

## Future Per-Script Environment Providers

Devshell support is explicitly outside this redesign's implementation scope, but
the hosting architecture must allow it without launching Kraai inside the
devshell.

### Required seam

Add a future environment-provider stage between effective request construction
and sandbox launch:

```text
effective script request
  -> configured ExecutionEnvironmentProvider
  -> provider wraps or materializes the child environment
  -> sandbox launch plan
  -> absolute pinned kraai-nushell-host
```

The provider receives a fully constructed inner launch plan and may either wrap
that command or return an environment delta. It never receives model-authored
shell text to concatenate.

Potential providers:

- `direct`: current profile environment/PATH behavior.
- `nix-develop`: conceptually
  `nix develop <installable> --command <sandbox-launch> <host> ...`.
- `direnv-exec`: conceptually
  `direnv exec <workspace> <sandbox-launch> <host> ...`.
- `direnv-export`: capture the JSON environment delta and apply it to the
  sandbox launch without a persistent shell hook.

The exact provider names and configuration are future work.

### Ordering and security requirements

- Environment preparation wraps the sandbox launch; the model script still runs
  inside `kraai-sandbox`.
- The pinned host is invoked by absolute path so a devshell PATH cannot replace
  Nushell or the Kraai runner.
- Nix-specific store/daemon mounts remain behind the already agreed explicit
  configuration flag.
- Devshell PATH entries may introduce new runtime roots. The Nix integration
  resolves those store paths into generic read-only runtime-root inputs.
- `.envrc` and Nix shell hooks are executable code. A future provider design
  must define trust/approval and must not silently execute an unapproved project
  environment outside the agent sandbox.
- Provider output becomes the base execution environment for only that script;
  it never mutates the long-running Kraai parent environment.
- Caching and invalidation must key off provider inputs such as `flake.lock`,
  the selected devshell, `.envrc`, and direnv approval state.

The selected embedded child makes this seam clean because the entire Nushell
appliance is one absolute executable that can be launched inside any prepared
environment.

## Production Acceptance Gate Before Full Migration

Implement the first production-shaped vertical slice before deleting the old
tool stack. The host, runtime, sandbox, IPC, and command abstractions created by
this gate are the components that will ship; this is not throwaway prototype
code:

1. Build a dedicated child with the pinned Nushell engine and normal command
   set.
2. Register dummy native Kraai commands returning both a structured record and
   a lazy stream of records.
3. Pass exact source over a private descriptor and run it inside the current
   restricted sandbox.
4. Prove an external child cannot inherit or forge the trusted event channel.
5. Pipe dummy command records through native filters and prove values remain
   structured without a parent round trip or eager collection.
6. Prove partial downstream consumption stops upstream work and cancellation
   propagates into a running native command.
7. Prove a completed dummy state effect reaches the parent before its success
   record becomes visible downstream and before a later statement times out.
8. Compare representative behavior with clean pinned `nu`.
9. Wrap the sandbox launch with a trivial environment-modifying command to prove
   the future environment-provider seam without implementing Nix or direnv.

Failure of this gate blocks the migration and returns the selected design for
explicit review. It does not authorize a plugin or generated-helper fallback.

## Sources Used For This Decision

- [Nushell native `Command` trait](https://docs.rs/nu-protocol/latest/nu_protocol/engine/trait.Command.html)
- [Nushell plugin developer documentation](https://www.nushell.sh/contributor-book/plugins.html)
- [Nushell plugin user and registry documentation](https://www.nushell.sh/book/plugins.html)
- [Nushell 0.114 crate composition](https://docs.rs/crate/nu/0.114.0)
- [Nix `develop --command` reference](https://nix.dev/manual/nix/2.30/command-ref/new-cli/nix3-develop.html)
- [direnv `exec` and `export` reference](https://direnv.net/man/direnv.1.html)
