# kraai-types review

Scope: `crates/kraai-types` only, with cross-crate call-site checks where those types define persisted/runtime contracts.

## Findings

### High: persisted wire format is implicit and unversioned

- Location: `crates/kraai-types/src/lib.rs:24-39`, `41-47`, `72-80`, `168-173`; persisted directly by `crates/kraai-persistence/src/lib.rs:119-136`.
- Impact: `Message` is the on-disk schema, but enum variants use default externally tagged serde names (`Complete`, `Streaming`, `AutonomousUpTo`, etc.) and new fields are only partly protected with `#[serde(default)]`. Any rename, enum representation change, or required field addition can make old histories fail to load entirely. This is especially risky because the project prioritizes session restarts and reconnects.
- Suggested fix: define explicit stable serde names for persisted enums, add a top-level `schema_version` or versioned persisted DTO, and add round-trip/golden JSON tests for representative messages. If source-level names should remain free to change, keep separate persistence structs and convert to runtime structs.

### Medium: `ChatMessage` erases tool-call structure before provider normalization

- Location: `crates/kraai-types/src/lib.rs:6-10`, `82-87`, `193-201`; call sites `crates/llm-providers/kraai-provider-openai-chat-completions/src/messages.rs:6-22` and `crates/llm-providers/kraai-provider-openai-codex/src/messages.rs:27-66`.
- Impact: provider-facing history is only `{ role, content }`, while `ToolCall` and `ToolResult` exist separately. Both OpenAI providers flatten tool results into user text with `"[Tool Result]\n..."`. That loses `call_id`, structured args/output, and tool role semantics, and makes provider adapters duplicate policy about how tool messages should be represented. It also makes it harder to use providers that support native tool calls or require strict message shapes.
- Suggested fix: replace or supplement `ChatMessage` with a structured content enum, for example `MessageContent::Text`, `ToolCall`, `ToolResult`, `ToolError`, with provider adapters deciding how to degrade unsupported content. At minimum, move the common "tool result as user text" formatting into `kraai-types` or provider-core so adapters cannot drift.

### Medium: tool state deltas are stringly typed JSON with silent data loss

- Location: `crates/kraai-types/src/lib.rs:203-214`; consumer `crates/kraai-agent/src/tool_state.rs:127-190`.
- Impact: `ToolStateDelta` carries `namespace`, `operation`, and arbitrary `serde_json::Value`. Unknown namespaces, unknown operations, and malformed payloads are silently ignored. That makes corrupted or incompatible persisted state hard to diagnose and can create misleading context after restarts. This is a maintainability smell because every namespace must hand-roll parsing and error handling.
- Suggested fix: introduce typed delta enums for built-in namespaces, or at least a `ToolStateDeltaKind` with validated constructors and fallible deserialization. Change snapshot resolution to return warnings/errors for malformed known namespaces, and add tests for invalid operation/payload handling.

### Medium: cancellation state exists but is not used consistently

- Location: `crates/kraai-types/src/lib.rs:41-47`; runtime behavior at `crates/kraai-agent/src/manager/streaming.rs:404-425`.
- Impact: `MessageStatus::Cancelled` exists, but cancelling a non-empty stream persists it as `Complete` (`streaming.rs:413-416`). `ProcessingTools` is also present as a type-level state, but I did not find active assignments. Dead or inconsistently used states make UI/recovery code harder to reason about and can hide partial output as complete output.
- Suggested fix: either remove unused statuses or make the lifecycle use them. For cancellation, persist non-empty cancelled assistant messages as `Cancelled` and update TUI rendering/fingerprinting accordingly, or delete the enum variant if the intended product behavior is "partial output is complete history".

### Medium: `TokenUsage::used_context_tokens` likely double-counts provider totals

- Location: `crates/kraai-types/src/lib.rs:49-70`; UI/runtime use `crates/kraai-runtime/src/api.rs:20-23`, `crates/kraai-tui/src/app/ui.rs:168-212`.
- Impact: the struct stores both `total_tokens` and component fields. `used_context_tokens` ignores `total_tokens` and sums input, output, reasoning, and cache-read tokens. Depending on provider semantics, reasoning tokens may already be included in output tokens and cache-read tokens may be a subset of input tokens. This can overstate context usage and display misleading pressure against `max_context`.
- Suggested fix: document exact semantics for each field and normalize provider usage into non-overlapping fields. If the goal is "model context consumed", prefer `total_tokens` when present, or compute `input_tokens + output_tokens` unless provider-specific fields are known to be additive. Add provider-normalization tests for OpenAI chat and Responses usage.

### Low: enum/string conversions are duplicated and incomplete

- Location: `RiskLevel::as_str/parse` in `crates/kraai-types/src/lib.rs:103-124`, `AgentProfileSource::as_str` in `126-141`, provider role conversion in `crates/llm-providers/kraai-provider-openai-chat-completions/src/messages.rs:25-42`.
- Impact: `RiskLevel` has manual parse/as-string logic but serde still emits variant names unless separately annotated. `AgentProfileSource` has `as_str` but no parser. `ChatRole` has serde renames, yet providers maintain separate conversion functions and one fallback maps unknown roles to `User` (`messages.rs:34-41`), which can mask invalid provider responses.
- Suggested fix: use `serde(rename_all = "snake_case")` where the JSON contract should be snake case, implement `FromStr`/`Display` for user-facing enums, and make provider `role_from_wire` return `Result<ChatRole>` instead of defaulting unknown roles to `User`.

### Low: important shared types lack equality derives and focused tests

- Location: `ChatMessage` at `crates/kraai-types/src/lib.rs:6-10`, `Message` at `24-39`, `ToolCall` at `82-87`, `ToolResult` at `193-201`, no tests under `crates/kraai-types`.
- Impact: many structs derive `Serialize`/`Deserialize` but not `PartialEq`/`Eq`, which makes direct round-trip tests awkward and pushes testing into downstream crates. The crate has no unit tests despite owning IDs, risk ordering, approval policy, token accounting, and persisted message schema.
- Suggested fix: derive `PartialEq`/`Eq` where fields permit it and add a small test module covering ID serde, `RiskLevel` ordering, `ToolCallAssessment::is_auto_approved`, `format_tool_result_message`, `TokenUsage` semantics, and message JSON compatibility.

## Refactor opportunities

- Split `src/lib.rs` into focused modules: `ids`, `message`, `usage`, `tools`, `profiles`, and `risk`. The current file is small, but this crate is a central contract crate and will become a god file quickly.
- Consider making `ToolId`, `ProviderId`, and `ModelId` lightweight `SmolStr`/`Box<str>`/`Arc<str>` wrappers instead of `Arc<String>`. `Arc<String>` adds an extra allocation layer and exposes no mutation benefit. This is not urgent, but if IDs are cloned heavily under load, benchmark the alternatives.
- Add a crate-level policy comment documenting which types are persisted wire format versus runtime-only DTOs. Right now that boundary has to be inferred from persistence and provider call sites.
