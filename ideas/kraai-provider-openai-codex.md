# kraai-provider-openai-codex review

Scope: `crates/llm-providers/kraai-provider-openai-codex`, with light cross-checks against `kraai-provider-core`, the OpenAI chat provider, runtime registration, and existing idea reports.

## Findings

### High: OAuth callback listener has no timeout and can leave login pending forever

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:281-318`, `553-629`.
- Impact: `start_browser_login` spawns `run_browser_login`, which loops on `listener.accept().await` with no deadline. If the user opens the browser flow and never returns, or the browser silently fails before hitting `/auth/callback`, `ControllerState.pending` remains `BrowserPending` indefinitely. The only cleanup path is explicit cancellation or starting a new login. That violates predictable failure/restart behavior and can leave the TUI displaying a stale pending state.
- Suggested fix: add a browser-login timeout similar to `DEVICE_CODE_TIMEOUT_SECS`, using `tokio::time::timeout` around the accept loop or a deadline checked around each accept/read. On timeout, call `finish_failed_login("OpenAI browser login timed out")` and emit status. Add a `tokio::test` using a loopback listener that never receives a callback.

### High: non-401 token refresh failures are not persisted to auth status

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:408-498`.
- Impact: `refresh_request_auth` clears auth and updates `state.error` only for `401 Unauthorized` (`auth.rs:435-440`) and account mismatch (`465-472`). For common retryable/non-retryable failures such as 429, 500, network errors, invalid JSON, or a token response without an account id, the method returns an error but leaves the stale auth in memory and leaves status as `Authenticated`. A later request will retry the same broken refresh path, while the provider screen may not show the real error.
- Suggested fix: centralize refresh failure handling. For permanent auth failures, clear auth with a user-facing error. For transient failures, preserve auth but store `state.error = Some(...)` and emit a status update so the UI can surface the failure. Add tests with a local HTTP server for 500, malformed token JSON, missing account id, and network failure.

### High: concurrent requests can stampede token refresh

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:382-405`, `408-498`; request caller at `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:438-463`.
- Impact: `get_request_auth` drops the state mutex before calling `refresh_request_auth`, and `refresh_request_auth` clones `old_auth` before sending the network refresh request. If many provider requests arrive after `TOKEN_REFRESH_INTERVAL_SECS` or many return 401 at once, every task can concurrently use the same refresh token. OAuth providers often rotate refresh tokens, so parallel refreshes can invalidate each other, cause spurious sign-outs, or waste rate limit.
- Suggested fix: guard refresh with a dedicated mutex or in-flight shared future. Re-check state after acquiring the refresh lock so only one request refreshes and the rest reuse the new tokens. Add a concurrency test using a local token endpoint and N simultaneous `get_request_auth` calls, asserting one refresh request.

### High: SSE stream never reports incomplete streams that end without terminal event

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/sse.rs:17-49`; stream mapping at `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:226-253`.
- Impact: `forward_sse_events` treats EOF as successful completion and flushes any partial event. The provider only emits usage on `"response.completed"` and errors on `"response.failed"` or `"response.incomplete"`. If the HTTP stream closes before any terminal event, callers receive a clean end-of-stream with no indication that output may be truncated and no usage. Under flaky networks this can persist partial assistant output as if the model completed normally.
- Suggested fix: have the stream parser or provider layer track whether a terminal event was observed. If EOF happens before `response.completed`, `response.failed`, `response.incomplete`, or `[DONE]`, emit an error such as `OpenAI response stream ended before completion`. Add tests for premature EOF after one delta and for normal EOF after `response.completed`.

### Medium: streaming errors discard useful OpenAI failure details

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/wire.rs:51-59`, `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:235-246`.
- Impact: `ResponsesStreamEvent` only deserializes `type`, `delta`, and completed `usage`. On `"response.failed"` or `"response.incomplete"`, the provider returns the fixed message `"OpenAI response stream failed"` and ignores any error object, incomplete reason, response id, or status payload the API sent. This makes auth/quota/model errors hard to debug and weakens telemetry.
- Suggested fix: extend the wire DTO with optional `error`, `incomplete_details`, and response id/status fields, then include them in the returned error. Add fixture-based tests for failed and incomplete events.

### Medium: non-streaming responses can silently return an empty assistant message

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:198-215`, `589-599`; DTOs at `crates/llm-providers/kraai-provider-openai-codex/src/wire.rs:91-111`.
- Impact: `generate_reply` extracts only `output[].type == "message"` and `content[].type == "output_text"`, then joins all text. If the Responses API returns a failed/incomplete response, refusal, alternative content shape, or no text, the caller receives `ChatMessage { role: Assistant, content: "" }` with no error. Empty assistant turns can pollute history and make upstream recovery harder.
- Suggested fix: deserialize response `status`, `error`, and incomplete details. Return an error for failed/incomplete statuses and for successful responses with no output text unless empty output is explicitly expected. Add unit tests for no message content, failed status, and mixed text/refusal content.

### Medium: model discovery prefers stale bundled context over fresher remote metadata

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:378-382`; catalog constants at `crates/llm-providers/kraai-provider-openai-codex/src/catalog.rs:61-109`.
- Impact: `expand_catalog_model` resolves `max_context` as variant config, base config, bundled catalog, then remote metadata. Because every visible catalog model currently has `Some(272_000)`, remote `max_context` is ignored. If ChatGPT changes a model's context window, discovery still advertises the stale bundled value. That can produce over-budget requests or unnecessarily conservative UI limits.
- Suggested fix: prefer explicit user config first, then remote metadata, then bundled fallback. Add a test where remote discovery returns `max_context: Some(111)` and assert the listed model uses 111.

### Medium: model catalog is hard-coded and will age quickly

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/catalog.rs:61-109`; matching behavior at `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:334-362`, `497-536`.
- Impact: discovery intersects the remote model list with hard-coded slugs. If a new Codex model appears remotely before this crate is updated, it is hidden. If no remote slug matches the local catalog, the provider falls back to all bundled models, which may include models the account cannot actually use. This undermines discovery reliability for a subscription-backed API where model availability changes server-side.
- Suggested fix: make the remote list authoritative for visibility and use the local catalog only as metadata for known models. Unknown remote Codex models should be listed with a default display name and no reasoning variants unless the API exposes efforts. If fallback to bundled models remains, mark it as degraded in logs/status.

### Medium: duplicated SSE implementation is already drifting risk

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/sse.rs:1-132` and `crates/llm-providers/kraai-provider-openai-chat-completions/src/sse.rs:1-132`.
- Impact: the SSE parser is byte-for-byte duplicated between two providers. Any fix for premature EOF, event fields, comments, retry lines, or buffer limits must be applied twice. This is a maintainability smell in provider plumbing and conflicts with the repo priority to extract shared logic instead of duplicating it.
- Suggested fix: move the SSE parser into `kraai-provider-core` as a shared helper, or create an internal shared OpenAI provider utility crate. Include parser tests once at the shared layer.

### Medium: SSE parser has unbounded line/event buffering

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/sse.rs:22-41`, `67-70`, `82`.
- Impact: `buffer` and `event_lines` grow without a maximum until a newline/blank line arrives. A buggy server, proxy, or malicious endpoint can force unbounded memory growth by streaming a very long line or many `data:` lines without an event terminator. This is less likely against the fixed ChatGPT endpoint, but provider code should stay robust under partial streams and reconnects.
- Suggested fix: enforce reasonable maximum line and event payload sizes, return a stream error when exceeded, and cover this with parser tests.

### Medium: auth file persistence is atomic-ish but not durable

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:901-924`, `938-949`.
- Impact: `persist_auth_file` writes a temp file, sets permissions, and renames it, but never fsyncs the temp file or parent directory. A crash or power loss can leave an empty/missing auth file after a successful login or token refresh. This matters because session restarts are a stated reliability priority.
- Suggested fix: after writing, open and `sync_all` the temp file, then after rename open the parent directory and sync it on Unix. Keep Windows behavior separately guarded. Add tests for permission behavior where possible; durability itself is hard to test but the code path can be covered.

### Low: `read_http_request` only reads one 4 KiB chunk

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:1018-1022`.
- Impact: the OAuth callback request is usually small, but the code assumes the request line and query string arrive in the first 4096 bytes. A long callback URL, browser/proxy behavior, or TCP segmentation could produce a partial request and cause missing code/state parsing. It is also a hand-rolled HTTP parser in an auth path.
- Suggested fix: read until `\r\n\r\n` with a max header size, or use a minimal HTTP server library if already available. Add tests for split request lines and oversized headers.

### Low: error logs can include sensitive auth/provider response bodies

- Location: `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:550-587`; auth errors at `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:432-444`, `747-752`.
- Impact: failed provider and auth responses are logged or returned with full body text. Some server errors include request ids, account details, or token-related diagnostics. This may be acceptable during alpha, but provider errors often get surfaced in TUI logs and should not leak sensitive data by default.
- Suggested fix: add a small sanitizer/truncator for response bodies before logging or returning errors. Preserve full bodies only behind debug tracing or an explicit diagnostic mode.

### Low: tests skip silently when reqwest cannot load system CAs

- Location: repeated helpers in `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:607-641` and `crates/llm-providers/kraai-provider-openai-codex/src/auth.rs:1062-1094`.
- Impact: several tests return early when `Client::builder().build()` fails with local CA issues. That keeps unusual Nix environments green, but it also means core local-only logic such as model discovery expansion and header construction can be skipped entirely. The skip predicate also treats `display == "builder error"` as missing CAs, which can hide unrelated builder failures.
- Suggested fix: split tests so pure logic does not construct `reqwest::Client` or auth controllers. For request-header tests, use a client builder configured with a known local root/cert setting if needed. Narrow the skip condition to known CA error sources rather than the generic `"builder error"` display.

## Refactor opportunities

- Split `auth.rs` into focused modules: controller state/status, browser login, device-code login, token exchange/refresh, token file persistence, and JWT parsing. At 1,257 lines, it is already a god file for an auth subsystem, and auth bugs are high-impact.
- Introduce provider-local integration fixtures with a tiny loopback HTTP server for auth refresh, model discovery, non-streaming Responses JSON, and streaming SSE. Most current tests are pure helper tests; the end-to-end request behavior around retries, refresh, and stream terminal states is not covered.
- Move common OpenAI request plumbing into shared code with the chat-completions provider: SSE parsing, retry/body error formatting, CA-safe test helpers, and tool-result degradation currently duplicate patterns.
- Replace stringly stream event matching (`event.kind.as_str()`) with typed enums for known event types plus an `Unknown(String)` fallback. That would make missing event handling easier to audit and test.
