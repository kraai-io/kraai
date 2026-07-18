# Final Cutover Checklist

> Status: mechanical completion checklist. This is used after the production
> components in document 05 pass the test gates in document 06. It does not
> authorize a partial release, dual protocol, or fallback.

## 1. Preconditions

- [ ] Final TOON baseline revision and evaluation artifacts are recorded.
- [ ] Production `kraai-sandbox` replaces the command-runner god file.
- [ ] Embedded `kraai-nushell-host` passes the production acceptance gate.
- [ ] Script protocol and capability policy tests pass.
- [ ] Initial native Kraai command crates pass focused and host integration
      tests.
- [ ] Persistence handshake and restart recovery pass fault injection.
- [ ] `<tool_call_result>` rendering and provider normalization are locked and
      snapshot-tested.
- [ ] Runtime, prompt, profiles, providers, and TUI are ready to switch in one
      coordinated cutover.
- [ ] No unresolved design question would require preserving an old runtime
      path.

If any precondition fails, stop. Do not keep or introduce a plugin, helper,
external-Nushell, TOON, or provider-native-tool fallback.

## 2. Workspace Members And Dependencies

Add final workspace members:

- [ ] `kraai-script-protocol`
- [ ] `kraai-sandbox`
- [ ] `kraai-nushell-runtime`
- [ ] `kraai-command-core`
- [ ] `kraai-workspace-fs`
- [ ] `kraai-command-open-files`
- [ ] `kraai-command-close-files`
- [ ] `kraai-command-edit-file`

Delete workspace members:

- [ ] `kraai-command-runner` after all sandbox callers have moved.
- [ ] `kraai-toon-schema`
- [ ] `kraai-tool-core`
- [ ] `kraai-tool-bash`
- [ ] `kraai-tool-read-file`
- [ ] `kraai-tool-list-files`
- [ ] `kraai-tool-search-files`
- [ ] `kraai-tool-open-file`
- [ ] `kraai-tool-close-file`
- [ ] `kraai-tool-edit-file`

Dependency cleanup:

- [ ] Add all Nushell crates at one exact full version.
- [ ] Keep every Rust dependency version as a full triple.
- [ ] Remove `toon-format` and TOON macro/parser dependencies.
- [ ] Remove old grep/search libraries with no surviving caller.
- [ ] Remove old tool crate path dependencies from every manifest.
- [ ] Check normal, dev, build, target-specific, feature-gated, example, and test
      dependencies—not only root workspace dependencies.
- [ ] Run `cargo metadata` and verify there is one intended command/runtime graph.

## 3. Shared Types

Add/retain:

- [ ] `ScriptExecutionId`
- [ ] `CommandInvocationId`
- [ ] `SandboxCapability`
- [ ] Capability closure/subsumption implementation
- [ ] `SandboxPermissionSet`
- [ ] Per-capability permission rule type
- [ ] `EscalationPolicy::{Deny, Prompt, Allow}`
- [ ] Immutable requested/effective script execution values
- [ ] Stable script status enum
- [ ] Structured script result/output channel types
- [ ] Internal `ToolCallResult` history role
- [ ] Renamed context state snapshot/effect types

Delete:

- [ ] `ToolCall`
- [ ] `ToolId`
- [ ] `ToolResult`
- [ ] `RiskLevel`
- [ ] `ExecutionPolicy`
- [ ] `ToolCallAssessment`
- [ ] Old `SandboxPermissions` and sandbox-mode vocabulary
- [ ] Old named-tool result formatters

- [ ] Verify `kraai-types` contains no parser, process, persistence, prompt, TUI,
      Nushell engine, or platform sandbox implementation.

## 4. Protocol And Prompt

- [ ] Replace regex/TOON parsing with `kraai-script-protocol` streaming state
      machine.
- [ ] Require exactly one `<tool_call>` block per assistant response.
- [ ] Preserve and stream ordinary assistant prose before the opening tag.
- [ ] Require a valid model-specified timeout.
- [ ] Parse optional requested capabilities from the start-tag attribute.
- [ ] Stop the provider at the first complete closing tag.
- [ ] Discard all trailing bytes, including bytes already buffered in that chunk.
- [ ] Reject unknown/duplicate attributes, capabilities, and malformed input.
- [ ] Ensure only Nushell parses the exact script bytes.
- [ ] Replace tool protocol/schema prompt text with the Nushell block protocol.
- [ ] Generate active command help/examples from command declarations.
- [ ] Explain `<tool_call_result>` and untrusted output.
- [ ] Remove all TOON examples and provider-native tool assumptions.

## 5. Profiles And Policy

- [ ] Replace profile tool IDs with native Kraai command IDs.
- [ ] Replace numeric risk thresholds with default sandbox permission sets.
- [ ] Add independent per-capability permission rules.
- [ ] Add fallback escalation policy.
- [ ] Resolve rule precedence before fallback policy.
- [ ] Aggregate the whole script into deny, one prompt, or allow.
- [ ] Make already-granted requested capabilities no-ops.
- [ ] Enforce exclusive `no-sandbox`.
- [ ] Configure final plan profile with open/close and read-only defaults.
- [ ] Configure final coding profile with edit and workspace-write defaults.
- [ ] Set reasonable initial environment allow-list, inherited/prepended PATH,
      clean Nushell startup, and workspace-root cwd.
- [ ] Ensure profile command absence prevents registration in the host.

## 6. Sandbox And Host

- [ ] Launch an absolute packaged `kraai-nushell-host` path.
- [ ] Create one immutable request and fresh authentication secret per script.
- [ ] Establish sandbox before model-authored source is parsed/evaluated.
- [ ] Pass the exact script through the framed private-temp socket, not argv or
      shell interpolation.
- [ ] Consume exactly one request frame before evaluation.
- [ ] Keep stdout and stderr separate from framed events and acknowledgments.
- [ ] Keep the dedicated transport descriptor close-on-exec and make it the only
      restricted-mode `connect` exception.
- [ ] Deny descendant tracing/process-memory inspection.
- [ ] Create one fresh Nu engine for one script and never pool initially.
- [ ] Register only active commands before parsing exact source.
- [ ] Load no user config, history, hooks, autoload directories, or plugins.
- [ ] Preserve native pipeline values, streams, backpressure, and cancellation.
- [ ] Keep external commands within the same owned sandbox/process tree.
- [ ] Start timeout immediately before host launch and exclude approval time.
- [ ] Fail closed when promised isolation cannot be established.
- [ ] Preserve process ownership and timeout even under `no-sandbox`.

## 7. Native Commands And Context State

- [ ] Extract reusable filesystem behavior into `kraai-workspace-fs` without
      command or policy dependencies.
- [ ] Generate runtime signature, prompt docs/examples, capability metadata, and
      registration from one declarative command declaration.
- [ ] `kraai-open-files` validates and pins paths without returning contents.
- [ ] `kraai-close-files` removes paths from future injected context.
- [ ] `kraai-edit-file` preserves validated create/edit semantics.
- [ ] Use normal Nushell/external commands for immediate reads/list/search.
- [ ] Emit authenticated sequenced effects while the script is running.
- [ ] Persist and acknowledge each completed effect before reporting its
      corresponding command success.
- [ ] Preserve acknowledged effects across later failure, timeout, cancellation,
      and restart.
- [ ] Refresh opened-file contents from disk before future turns.
- [ ] Keep one command implementation crate per model-facing command.
- [ ] Remove all legacy command aliases and old tool adapters.

## 8. Persistence And Recovery

- [ ] Persist exact assistant preamble and `<tool_call>` source.
- [ ] Persist immutable request metadata and profile snapshot identity.
- [ ] Persist live stdout/stderr without compaction or truncation.
- [ ] Persist acknowledged context effects before final status.
- [ ] Persist exactly one terminal status.
- [ ] Render one `<tool_call_result>` from structured status/channels.
- [ ] Include only applicable exit code and nonempty output sections.
- [ ] Keep result-looking output inert and exact.
- [ ] Store result role distinctly from human user messages.
- [ ] Reconstruct context state and opened files after restart.
- [ ] Never replay arbitrary interrupted scripts automatically.
- [ ] Prevent duplicate continuation after recovery.
- [ ] Reject old TOON sessions clearly; add no migration or fallback reader.
- [ ] Split message, execution, context-state, and turn persistence modules.

## 9. Runtime And Providers

- [ ] Replace pending tools, batches, and per-tool approvals with one script
      execution state machine.
- [ ] Make illegal lifecycle combinations unrepresentable.
- [ ] Show one whole-script approval containing exact source and capability diff.
- [ ] Implement allow once and deny once only.
- [ ] Persist denial without launching the host.
- [ ] Continue exactly once for every agreed status except user cancellation.
- [ ] Preserve queueing, workspace guards, provider retries, and session shutdown
      outside the new script modules.
- [ ] Map internal `ToolCallResult` to ordinary provider input text.
- [ ] Send the exact `<tool_call_result>` block with no extra prefix.
- [ ] Send no native tool schemas, tool calls, function outputs, or synthetic IDs.
- [ ] Test complete outbound message role/order/content for every provider.
- [ ] Remove tool batch outcomes and per-tool runtime/TUI events.

## 10. TUI

- [ ] Render assistant prose and one Nushell source block.
- [ ] Render whole-script approval and capability difference.
- [ ] Render running state and live stdout/stderr.
- [ ] Support user cancellation of the owned execution tree.
- [ ] Render all stable terminal statuses and duration.
- [ ] Preserve complete large output with navigation/scrolling.
- [ ] Show useful nested command details without cluttering normal chat.
- [ ] Distinguish tool-call results from human messages internally and visually.
- [ ] Remove TOON decoding, named-tool cards, arguments, risk displays, pending
      tool polling, and batch branches.

## 11. Nix And Runtime Packaging

- [ ] Package `kraai-nushell-host` with Kraai.
- [ ] Package matching clean `nu` where required for conformance/development.
- [ ] Package Bubblewrap and the intended command PATH, including `rg`.
- [ ] Add host/Nushell/commands as explicit read-only runtime roots.
- [ ] Update NixOS test inputs for every runtime executable used by tests.
- [ ] Keep Nix store/daemon features behind explicit configuration.
- [ ] Verify Kraai starts outside a devshell with an empty user Nushell config.
- [ ] Update application wrappers and runtime closures.
- [ ] Regenerate `Cargo.nix` after final Cargo graph changes.
- [ ] Verify general Linux builds contain no unconditional Nix policy.

## 12. Repository-Wide Deletion Audit

Run repository searches after deletion. Every remaining match must be a plan,
baseline artifact, or explicitly justified historical documentation—not runtime
code, tests, prompts, manifests, Nix outputs, or generated Cargo data.

Search concepts:

```text
toon_tool
kraai-toon-schema
toon-format
TypedTool
ErasedTool
PreparedToolCall
ToolManager
ToolBatchOutcome
PendingTool
RiskLevel
ToolCallAssessment
format_tool_result_message
[Tool Result]
kraai-tool-
tool: bash
tool: read_file
tool: list_files
tool: search_files
```

Also inspect:

- [ ] Root and crate `Cargo.toml` files.
- [ ] `Cargo.lock` and generated `Cargo.nix`.
- [ ] Nix package definitions and application wrappers.
- [ ] Prompt snapshots and evaluation fixtures.
- [ ] TUI snapshots and event enums.
- [ ] Provider normalizers.
- [ ] README and user-facing examples.
- [ ] Ignored generated/evaluation files where stale runtime assumptions could
      affect active tests.

Do not weaken the audit by adding aliases, deprecated shims, ignored dead-code
modules, optional legacy features, or fallback deserializers.

## 13. Final Verification Commands

Read the live `justfile` before execution and use its current commands. The
expected final sequence is:

1. Focused tests for every new/changed crate.
2. Embedded-host conformance and real sandbox integration tests.
3. `just check` for generated Cargo/Nix metadata, formatting, lint, and tests.
4. Relevant Nix builds and check derivations.
5. Full Nix flake checks when the focused derivations pass.
6. Final open/close and multi-operation Nushell evaluation suites.
7. Repository-wide deletion searches.
8. Clean-workspace application launch and smoke test.

- [ ] Distinguish infrastructure/daemon failures from real derivation or test
      failures before rerunning.
- [ ] Inspect the actual failing Nix derivation/log when local tests disagree.
- [ ] Record final token, latency, correctness, and closure-size comparison.
- [ ] Confirm working tree contains only intended migration changes and generated
      files.

## 14. Completion And Rollback Rule

The cutover is complete only when all applicable checkboxes and document 06
release gates pass and the old runtime no longer exists in the workspace.

If a blocker is discovered before release, fix the production architecture or
return to design review. If the branch must be abandoned, recover the previous
implementation from version-control history. Do not retain a runtime toggle,
compatibility crate, plugin/helper fallback, or second model-facing protocol as
an in-product rollback mechanism.
