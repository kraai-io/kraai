# kraai-provider-openai-chat-completions findings

Scope: `crates/llm-providers/kraai-provider-openai-chat-completions`

## High severity

1. Configured models disappear after model discovery unless the remote `/models` endpoint returns them.
   - References: `src/provider.rs:101-122`, `src/provider.rs:127-144`, `src/profile.rs:52-60`
   - Impact: `register_model` stores model metadata, but `cache_models` clears the cache and only inserts models returned by `GET /models`. With `only_listed_models = true`, configured models are still filtered through the remote list. This means a manually configured model cannot be selected if the provider's model-list endpoint omits it, is stale, or hides preview/fine-tuned/deployment model IDs. It also contradicts the provider-field help text: "only models explicitly configured in providers.toml are shown".
   - Suggested fix: Treat configured models as the source of truth when `only_listed_models = true`: build the cache from `model_configs` directly, enriching from remote data only when available. When `only_listed_models = false`, merge remote models plus configured-only models. Add tests for configured-only, remote-only, and merged behavior.

2. Prompt cache keys are ignored by this provider.
   - References: `kraai-provider-core/src/http_retry.rs:47-91`, `src/provider.rs:153-158`, `src/provider.rs:186-193`, `src/wire.rs:3-11`
   - Impact: `ProviderRequestContext` carries `prompt_cache_key`, and the OpenAI Codex provider uses it in request payload/session headers. The chat-completions provider drops it entirely. If callers rely on this context for cache affinity or provider-side prompt caching, this provider silently loses the performance/cost benefit and behaves inconsistently with other providers.
   - Suggested fix: Decide the chat-completions protocol contract for prompt-cache affinity. If the target OpenAI-compatible APIs support a stable request field or header, add it to `ChatCompletionRequest`/request builder; otherwise document that this provider does not support `prompt_cache_key` and expose that capability explicitly so callers do not assume it works.

3. Streaming discards non-content chunks and cannot surface stream-level failure details.
   - References: `src/provider.rs:199-219`, `src/wire.rs:42-58`, `src/sse.rs:67-70`
   - Impact: The stream handler only emits usage or the first `delta.content`. It ignores `finish_reason`, multiple choices, role-only chunks, provider error payloads, and any SSE `event:` field. If an OpenAI-compatible server sends an error object as an SSE data payload, it is parsed as `ChatCompletionChunk` with no choices and silently dropped if the shape happens to deserialize, or reported as a generic JSON error if not. Consumers may see a cleanly ended stream even though the provider signaled a failure.
   - Suggested fix: Model stream events more fully: include `finish_reason`, optional error fields, and choice index. Parse SSE `event:` in `sse.rs` or at least detect JSON error envelopes before `ChatCompletionChunk` parsing. Emit an explicit error on provider failure events instead of dropping them.

## Medium severity

4. Non-streaming usage is parsed and then discarded.
   - References: `src/provider.rs:160-177`
   - Impact: `generate_reply` normalizes `response.usage` into `_usage`, but the returned `ChatMessage` has no place to carry it. Non-streaming calls therefore lose token accounting, while streaming can emit `ProviderStreamEvent::Usage`. This creates inconsistent accounting depending on streaming mode.
   - Suggested fix: Change the provider/core return contract so non-streaming generation can return both message and optional usage, or remove the dead parse until the contract supports it. Add a regression test once the contract is changed.

5. `cache_models` has no fallback path on discovery failure.
   - References: `src/provider.rs:86-124`, `kraai-provider-core/src/lib.rs:405-408`
   - Impact: `ProviderManager::load_config` calls `update_models_list`, which fails the whole provider load if `GET /models` fails. For OpenAI-compatible providers, model listing is often less reliable or less complete than inference. A transient discovery outage can make a configured provider unusable even when its configured model would work.
   - Suggested fix: If configured models exist, populate the cache from them when discovery fails and return `Ok(())` with a warning. This mirrors the more resilient pattern in `kraai-provider-openai-codex/src/provider.rs:156-175`, which falls back to a local catalog.

6. HTTP error bodies may leak secrets or huge payloads into user-visible errors.
   - References: `src/provider.rs:261-275`
   - Impact: `ensure_success_response` includes the full response URL and body in the error. Some compatible gateways echo request metadata, deployment names, auth diagnostics, or large HTML/error pages. This can expose sensitive data in logs/UI and produce noisy errors under load.
   - Suggested fix: Add a shared provider error formatter that truncates bodies, redacts obvious credential-looking values, and preserves structured provider error messages where safe. The Codex provider has similar logic at `kraai-provider-openai-codex/src/provider.rs:566-587`, so this should probably be shared in provider-core.

7. The SSE parser can grow memory without a maximum line/event size.
   - References: `src/sse.rs:22-48`, `src/sse.rs:67-83`
   - Impact: `buffer` and `event_lines` grow until a newline/blank line arrives. A broken or malicious endpoint can hold memory indefinitely by streaming a very long line or never flushing an event. This matters because providers are remote services and streams are long-lived.
   - Suggested fix: Enforce maximum line and event payload sizes, returning an error when exceeded. Add tests for overlong line and overlong multi-line event behavior.

8. Tool messages are flattened into user text, losing structure and tool-call identity.
   - References: `src/messages.rs:6-23`, `kraai-types/src/lib.rs:216-226`
   - Impact: `ChatRole::Tool` becomes a user message prefixed with `[Tool Result]`. This is pragmatic for simple chat APIs, but it loses the distinction between user intent and tool output, and can make prompt-injection boundaries weaker. It also means `role_to_wire(ChatRole::Tool)` returns `"tool"` at `src/messages.rs:25-31`, but `normalize_chat_messages` deliberately does something else.
   - Suggested fix: Either support proper OpenAI tool-result message shapes once `kraai_types` can carry tool call IDs, or make the flattening an explicit compatibility mode with tests and comments documenting the security tradeoff.

## Low severity / maintainability

9. Provider/client construction is hard to test without hand-built private structs.
   - References: `src/provider.rs:278-299`, tests at `src/provider.rs:480-493`
   - Impact: Tests must manually construct `ChatCompletionsProvider` to inject a local base URL and short-timeout client. As coverage grows, this encourages brittle test setup inside private modules.
   - Suggested fix: Add a small internal constructor or builder used by both `create_provider` and tests. Keep it private to the crate/module if the public API should stay narrow.

10. Model/config validation is under-tested.
    - References: `src/profile.rs:88-159`, `src/auth.rs:11-33`, `src/provider.rs:127-144`
    - Impact: The crate currently has tests for retry forwarding, usage normalization, and two SSE edge cases, but no tests for provider definitions, required base URL behavior, credential resolution precedence, invalid `only_listed_models`, invalid `max_context`, or registration/cache interactions.
    - Suggested fix: Add unit tests around `OpenAiChatCompletionsFactory::definition`, provider/model validation, `ApiKeyAuth::resolve`, and `register_model`/`cache_models` behavior with a local scripted `/models` endpoint.

11. Streaming tests do not cover realistic OpenAI chat-completion chunks.
    - References: `src/provider.rs:199-219`, `src/sse.rs:95-132`
    - Impact: Existing SSE tests only verify line/chunk parsing. There are no tests proving that chat-completion stream JSON emits text deltas, usage, `[DONE]` termination, multiple `data:` lines, malformed JSON errors, or provider error envelopes.
    - Suggested fix: Add provider-level streaming tests using a scripted HTTP server that sends `text/event-stream` responses with representative OpenAI chunks and failure cases.

12. Shared OpenAI-compatible provider logic is starting to duplicate across crates.
    - References: `src/provider.rs:225-276`, `src/profile.rs:135-159`, `kraai-provider-openai-codex/src/provider.rs:259-288`, `kraai-provider-openai-codex/src/provider.rs:101-125`, `kraai-provider-openai-codex/src/provider.rs:566-587`
    - Impact: Usage normalization, max-context validation, model metadata registration, success/error handling, and test helpers are repeated between OpenAI-family providers. This increases the chance that reliability fixes land in one provider but not the other.
    - Suggested fix: Extract small shared helpers into `kraai-provider-core` or a provider-local shared module, starting with validation helpers, token-usage normalization primitives, and bounded error-body formatting. Avoid a large abstraction until the common surface is clearer.

