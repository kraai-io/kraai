# kraai-types review

Scope: `crates/kraai-types` only, with cross-crate call-site checks where those types define persisted/runtime contracts.

## Findings

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
