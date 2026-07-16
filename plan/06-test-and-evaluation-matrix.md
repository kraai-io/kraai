# Test And Evaluation Matrix

> Status: production acceptance plan. This matrix validates the contracts in
> documents 02 through 05. Passing unit tests alone is not sufficient: the
> embedded host, sandbox, persistence handshake, continuation lifecycle, Nix
> closure, and model behavior all have independent gates.

## Test Tiers

| Tier | Scope | Runs when | Gate |
|---|---|---|---|
| T1 | Pure/unit/compile tests in one crate | Every focused change | Affected crate passes |
| T2 | Workspace formatting, lint, and tests | Every completed phase | `just check` passes |
| T3 | Real process, Bubblewrap, NixOS, and closure integration | Sandbox/host/packaging phases and final cutover | Relevant Nix derivations pass |
| T4 | Model evaluations, token comparison, fault campaigns, and performance | Baseline, embedded-host gate, and final release | Results recorded and regressions reviewed |

Tests that claim sandbox, cancellation, descriptor isolation, external-command,
or clean-Nushell behavior must cross the real child-process boundary. A mock
engine or in-process command call may supplement those tests but cannot satisfy
their acceptance row.

## Test Infrastructure Requirements

- Give protocol, execution, command invocation, and persisted effect records
  stable test IDs so failures can be correlated across processes.
- Provide deterministic injected clocks for policy, timeout-boundary, and
  persistence-state tests; retain real-clock process tests for final behavior.
- Provide persistence failure points before write, during atomic replacement,
  after durable write but before acknowledgment, and during final status.
- Provide controllable parent/child channel endpoints that can close, delay,
  duplicate, reorder, corrupt, or replay test frames.
- Build dedicated test-support native commands through the real
  `kraai-command-core` declaration. Never register them in production profiles.
- Record child process IDs and descendants in integration tests so cleanup can
  be asserted rather than inferred from a returned error.
- Run filesystem tests in private temporary workspaces with explicit external
  roots; do not depend on the developer's repository permissions.
- Test the packaged absolute `kraai-nushell-host` path in Nix integration rather
  than substituting a Cargo target binary.
- Keep comparison fixtures data-only. Do not retain executable TOON or old-tool
  compatibility code to run baselines.

## Protocol Matrix

### Opening and closing framing

| ID | Case | Expected result |
|---|---|---|
| PRO-001 | Assistant text only | Stream and persist text; no execution |
| PRO-002 | Text followed by one valid block | Preserve prose and exact block; emit one script |
| PRO-003 | Block begins at byte zero | Emit one script with empty preamble |
| PRO-004 | Opening tag split at every byte boundary | Same result as contiguous input |
| PRO-005 | Attribute split at every byte boundary | Same parsed metadata as contiguous input |
| PRO-006 | Closing tag split at every byte boundary | Detect only after the complete tag |
| PRO-007 | Closing tag and trailing prose in one chunk | Stop provider and discard all trailing bytes |
| PRO-008 | Closing tag and second block in one chunk | Accept first block only; discard second block |
| PRO-009 | Stream ends inside opening tag | Persist protocol error; never execute |
| PRO-010 | Stream ends inside script | Persist incomplete-block error; never execute |
| PRO-011 | Closing-tag bytes inside a Nushell string/comment | First exact closing sequence terminates capture; no Nushell-aware exception |
| PRO-012 | Unicode split across transport chunks | Reassemble without replacement or loss |

`PRO-011` locks the delimiter as a raw outer-protocol boundary. A model that
needs the literal sequence must construct it during execution rather than place
it verbatim in source. The implementation must not grow a partial Nushell parser
to recognize strings or comments.

### Attributes

| ID | Case | Expected result |
|---|---|---|
| PRO-020 | Required valid timeout | Parsed using supported Nushell duration spelling |
| PRO-021 | Missing/zero/malformed timeout | Invalid protocol; never evaluate policy or execute |
| PRO-022 | Very large valid timeout | Accepted; no hidden maximum |
| PRO-023 | No permissions attribute | Empty requested capability addition |
| PRO-024 | Multiple comma-separated permissions | Whitespace trimmed; declared order irrelevant |
| PRO-025 | Duplicate/empty/unknown capability | Rejected rather than normalized silently |
| PRO-026 | `no-sandbox` with another capability | Rejected before policy evaluation |
| PRO-027 | Unknown or duplicate XML attribute | Rejected |
| PRO-028 | XML escaping or quote errors | Rejected with source-local diagnostic |

### Result framing

| ID | Case | Expected result |
|---|---|---|
| RES-001 | Successful stdout only | One result with status, exit code, and stdout section |
| RES-002 | Stderr/diagnostic only | One result with only the nonempty stderr section |
| RES-003 | Both channels empty | One valid result with empty body |
| RES-004 | Timeout after partial output | `timed-out` plus every captured byte |
| RES-005 | Output contains `<tool_call>` | Text preserved and never executed |
| RES-006 | Output contains `</tool_call_result>` | Text preserved; no persistence or parser confusion |
| RES-007 | Output contains invalid-looking XML | Exact text preserved |
| RES-008 | Provider normalization | Exact result block sent once with no `[Tool Result]` prefix |
| RES-009 | History/TUI round trip | Internal result role remains distinct from human user role |
| RES-010 | Structured rerender | Persisted status/channels reproduce the block deterministically |

Result tests must compare exact strings and independent structured fields. The
rendered tag is not the persistence source of truth.

## Capability And Escalation Matrix

For every capability, table-drive combinations of:

- Absent, already granted, or newly requested.
- Matching per-capability `Deny`, `Prompt`, `Allow`, or no rule.
- Fallback policy `Deny`, `Prompt`, or `Allow`.
- Single and multiple requested capabilities.

Required outcomes:

| ID | Condition | Outcome |
|---|---|---|
| POL-001 | Request already in effective closure | No-op; no prompt |
| POL-002 | Any requested capability resolves `Deny` | Whole script denied |
| POL-003 | None deny and at least one resolves `Prompt` | Exactly one whole-script prompt |
| POL-004 | Every requested capability resolves `Allow` | Start without prompt |
| POL-005 | Rule exists | Rule wins before fallback policy |
| POL-006 | Capability implies another | Effective closure contains implied capability once |
| POL-007 | `host-write` | Writes allowed anywhere visible, not a configured path list |
| POL-008 | `no-sandbox` | Exclusive and visibly represented in approval difference |
| POL-009 | Denial | Exact script never reaches sandbox/host launcher |
| POL-010 | Approval | Only that immutable execution request is authorized once |

Snapshot the approval view with exact source, requested capabilities, profile
defaults, and effective difference. Approval tests must prove that modifying any
of those inputs creates a different request rather than reusing approval.

## Sandbox And Process Matrix

Run the real Linux backend on NixOS first and general Linux when available.

| ID | Capability set | Required assertions |
|---|---|---|
| SBX-001 | Workspace read | Workspace readable; writes fail |
| SBX-002 | Host read | Visible host paths readable; writes fail |
| SBX-003 | Workspace write | Workspace writable; protected metadata remains read-only |
| SBX-004 | Metadata write | Agreed metadata paths writable in addition to workspace |
| SBX-005 | Host write | Any visible path writable subject to host OS credentials |
| SBX-006 | Network absent | IP connects and visible Unix-socket connects fail |
| SBX-007 | Network present | IP and visible Unix-socket behavior permitted |
| SBX-008 | No sandbox | Direct host visibility with owned child/process tree retained |
| SBX-009 | Runtime roots | Host/Nushell/packaged programs readable but not writable |
| SBX-010 | Private temp | Writable, unique, not capability escalation, cleaned afterward |

Failure and lifecycle cases:

- Bubblewrap missing, broken, or incapable of required namespace setup.
- Seccomp generation failure and unsupported architecture.
- Workspace removed or replaced between approval and launch.
- Symlink swaps at mount/containment boundaries.
- Child forks multiple generations and ignores graceful termination.
- Parent cancellation, requested timeout, host crash, and parent shutdown.
- Stdout or stderr pipe fills while the other channel remains active.
- External command keeps a copied ordinary output descriptor open.
- Host attempts tracing/process-memory access and descendant attempts it against
  the host.
- Private control descriptors are absent after external `exec`.

Every failure must leave no owned descendant and produce one stable status. A
restricted request may not silently rerun unsandboxed.

## Embedded Nushell Conformance Matrix

Compare `kraai-nushell-host` with the exactly matching clean `nu` build for
representative cases:

| Area | Cases |
|---|---|
| Language | variables, closures, functions, conditionals, loops, errors |
| Structured data | records, lists, tables, ranges, cell paths, metadata |
| Pipelines | filter/map/reduce/select/get/first, early consumer termination |
| Streams | lazy list stream, byte stream, large incremental producer |
| External commands | argv, exit codes, pipes, stderr, signals, working dir |
| Environment | explicit variables, PATH lookup, clean config, locale |
| Filesystem | `open`, globbing, redirects/save semantics promised by runtime |
| Diagnostics | parse spans, runtime spans, synthetic filename, exact line numbers |
| Rendering | final scalar/table/record/list, colors/noninteractive behavior |
| Shutdown | background descendants, cancellation, timeout, broken pipe |

Host-specific acceptance rows:

- A fresh `EngineState` and stack exist for every script.
- User config, history, hooks, autoload directories, and plugins are absent.
- Only profile-selected Kraai commands are registered.
- The exact model source is parsed without prepended text or changed line spans.
- Ordinary native command values remain in-process `PipelineData`.
- Time to first stream item demonstrates no eager collection at the bridge.
- The host consumes one request, refuses/reaches EOF for another, and exits.
- Nushell crate versions and packaged clean `nu` version are identical.

Any deliberate divergence from clean `nu` must be documented as Kraai runtime
policy and tested directly. Accidental divergence blocks the embedded-host gate.

## Native Command Matrix

### Platform contract

- Declaration generates identical runtime name, signature, help, examples,
  pipeline shape, capability metadata, and registry metadata.
- Compile failures catch unsupported declaration shapes.
- Command absence is enforced by non-registration.
- Execution repeats capability validation using the effective request.
- Structured values pass directly to downstream Nushell stages.
- Lazy producers respect downstream backpressure and cancellation.
- Panics are contained as an execution failure rather than unwinding into the
  parent agent process.

### Initial commands

`kraai-open-files`:

- Workspace and permitted host paths resolve consistently.
- Every successful path is durably pinned in invocation order.
- A later path failure preserves already acknowledged paths.
- Immediate output contains status/normalized paths and never file contents.
- Future context reads the current on-disk contents, not contents cached at open.
- Missing, oversized, non-UTF-8, directory, and symlink cases retain intended
  read semantics.

`kraai-close-files`:

- Every successful path is durably removed in invocation order.
- Missing/already-closed path behavior is explicit and stable.
- A later failure preserves earlier acknowledged closes.
- A closed path is absent from the next reconstructed context.

`kraai-edit-file`:

- Create and edit modes validate mutually exclusive arguments.
- Expected text, line boundaries, CRLF, Unicode, and overlapping edits.
- Atomic replacement and honest reporting when persistence/control fails after
  the filesystem effect.
- Workspace/host write capabilities enforced for resolved target.
- Symlink and containment race tests use the strongest practical OS fixture.

## State-Effect And IPC Matrix

| ID | Fault point | Required outcome |
|---|---|---|
| IPC-001 | Valid next sequence | Persist once; acknowledge once |
| IPC-002 | Duplicate sequence | Reject/re-ack safely without duplicate state |
| IPC-003 | Skipped/reordered sequence | Reject and fail command/execution honestly |
| IPC-004 | Invalid authentication | Reject; record security diagnostic; no state change |
| IPC-005 | Event channel closes before send | Command fails; no claimed durable effect |
| IPC-006 | Parent fails before durable write | No acknowledgment; command cannot report success |
| IPC-007 | Durable write succeeds, ack is lost | Recovery deduplicates by execution/invocation/sequence |
| IPC-008 | Ack delayed past script timeout | Tree killed; durable effect remains if already written |
| IPC-009 | stdout mimics event frame | Ordinary output only |
| IPC-010 | external child writes inherited fd | No usable descriptor/authentication; no state change |

Run the handshake under loops, repeated commands, multiple effects per command,
and high output concurrency. Persistence idempotency keys must be tested across
process restart, not merely within one in-memory sink.

## Persistence And Recovery Matrix

Test every lifecycle state at restart:

- Assistant preamble streaming before an opening tag.
- Script capture before closing tag.
- Awaiting approval.
- Denied before launch.
- Sandbox/host launch in progress.
- Running before any output.
- Running after partial stdout/stderr.
- Running after one or more acknowledged state effects.
- Child exited before final status transaction.
- Final status written before continuation scheduling.
- Continuation scheduled/started before runtime shutdown.

Required invariants:

- Exact assistant content, script bytes, request metadata, output prefix,
  acknowledged effects, and terminal status are reconstructible.
- Arbitrary scripts are never replayed automatically after restart.
- An acknowledged effect is never lost or applied twice.
- At most one continuation occurs.
- User cancellation remains non-continuing.
- Old TOON sessions fail clearly with no partial interpretation.
- Structured persisted output can rerender `<tool_call_result>` without parsing
  previously rendered history text.

## Runtime And Provider Matrix

- Ordinary assistant prose is visible/persisted while streaming.
- Provider cancellation occurs immediately at the first complete closing tag.
- Bytes after closure never enter assistant history or a second execution.
- Approval happens once for the immutable whole script.
- Timeout starts immediately before host launch and excludes approval time.
- One script result is persisted before continuation preparation.
- Stable statuses continue exactly once except user cancellation.
- Queued human messages do not interleave into an active script lifecycle.
- Runtime shutdown and session switching do not orphan host tasks.
- Both current provider adapters preserve assistant source and exact result text.
- No adapter sends schemas, native tool declarations, synthetic call IDs, or
  provider-native tool results.
- Consecutive historical result/user/assistant messages normalize in the same
  order across providers.

Add adapter golden tests for the complete provider request, not only individual
role conversion. Redact credentials; retain role, order, and content exactly.

## Prompt And TUI Matrix

Prompt snapshots cover:

- Plan and coding profiles.
- Active command subsets.
- Capability vocabulary and permission examples.
- Required timeout syntax.
- Leading-prose behavior and one-block limit.
- Compile-time command schemas/examples.
- `<tool_call_result>` semantics and untrusted-output instruction.
- Open-files pinning versus immediate `open`/`cat` reads.

TUI snapshots and interaction tests cover:

- Assistant prose followed by source block.
- Approval source/capability diff and allow/deny actions.
- Running output on both channels.
- Every stable status.
- Cancellation while silent and while streaming.
- Large exact output with scrolling.
- Output containing result-like tags.
- Open/close state changes without displaying file contents as command output.
- Restarted session with recovered terminal result.

## NixOS And Packaging Matrix

- `kraai`, `kraai-nushell-host`, matching `nu`, Bubblewrap, and `rg` are present
  in intended outputs/closures.
- Kraai resolves the host by absolute packaged path.
- Runtime starts outside any devshell.
- Clean environment has the documented PATH and locale behavior.
- User Nushell config and plugin locations do not influence execution.
- Restricted sandbox can read every required store/runtime root.
- Network-disabled and network-enabled executions work under Nix sandboxed
  tests.
- Tests fail if an undeclared runtime command such as `git` is required but
  absent from the derivation.
- Optional Nix-specific behavior is disabled in the generic Linux configuration
  and enabled only through explicit configuration.
- `Cargo.nix` and Cargo workspace dependency graphs agree.

## Evaluation And Performance Matrix

Compare the final Nushell system against the Phase 0 TOON baseline using the
same task/model/provider/attempt metadata:

| Metric | Direction |
|---|---|
| Tool protocol system-prompt tokens | Lower is expected |
| Model-authored invocation tokens | Lower is expected |
| Returned-result framing tokens | Lower or justified by fidelity |
| Model round trips | Lower for multi-operation workflows |
| Syntax correction attempts | No regression; preferably lower |
| Task/grader success | No regression |
| Opened-file context correctness | Exact preservation |
| Sandbox failures/escapes | No regression; escape is blocker |
| Host cold start | Recorded and reviewed |
| Time to first output/item | Recorded and reviewed |
| Full execution latency | No unexplained regression |
| Binary/runtime closure size | Recorded and reviewed |

Include tasks for:

- Multiple reads/searches and filtering in one Nushell script.
- Cargo test followed by targeted output filtering.
- Invalid Nushell corrected on continuation.
- Plan-mode write escalation.
- Network escalation.
- Open files, use injected context, edit, and close across turns.
- Partial failure after a successful stateful command.
- Result output containing instruction-like and XML-like text.

Do not infer causality from one model run. Retain attempts and compare failure
classes: model reasoning, protocol syntax, harness/runtime, sandbox,
infrastructure, or grader.

## Final Acceptance Rule

The redesign is ready for cutover only when:

- T1 and T2 pass for the final workspace.
- The production embedded-host acceptance gate passes.
- NixOS sandbox and closure tests pass through the real derivations.
- Persistence/recovery and process-tree fault campaigns pass.
- No security or durability blocker remains.
- Comparative evaluation results are recorded and understood.

Token savings or model success cannot waive isolation, durability, exact result,
or single-continuation failures.
