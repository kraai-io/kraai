# kraai-provider-core Findings

Scope: `crates/llm-providers/kraai-provider-core`, with adjacent provider/runtime references only where they show concrete impact or extraction opportunities.

## High Severity

### `HttpRetryPolicy { max_attempts: 0 }` falls through to `unreachable!`

- Location: `crates/llm-providers/kraai-provider-core/src/http_retry.rs:94-159`
- Impact: `send_with_retry` iterates `1..=policy.max_attempts` and then panics via `unreachable!` if `max_attempts` is zero. The type is public and has no constructor/invariant, so any caller or future config path can crash the runtime instead of returning a validation error. This is especially sharp because retry policy tuning is likely to become user/config driven.
- Suggested fix: Enforce `max_attempts >= 1` at the API boundary. Either make fields private with a validating constructor, add a `NonZeroU32` field, or return a typed error from `send_with_retry` when the policy is invalid. Add a unit test for zero attempts.

## Medium Severity

### Default retry policy can sleep for many hours per request

- Location: `crates/llm-providers/kraai-provider-core/src/http_retry.rs:30-33`, backoff at `16-27`, sleeps at `129` and `153`
- Impact: `DEFAULT_HTTP_RETRY_POLICY` uses 20 attempts with uncapped exponential backoff starting at 1 second. Ignoring `Retry-After`, worst-case sleeps are `1 + 2 + ... + 2^18` seconds, over 145 hours before the final attempt returns. A single provider call can tie up a stream task/session for days, delaying recovery and making cancellation the only practical escape.
- Suggested fix: Add `max_backoff` and preferably `max_elapsed_time` to `HttpRetryPolicy`, then cap each delay and stop once the elapsed budget is exhausted. Consider a smaller default, plus operation-specific overrides for interactive chat versus background model refresh. Add tests for capped backoff and elapsed-budget termination.

### Retry storms are likely under provider outages

- Location: deterministic backoff in `crates/llm-providers/kraai-provider-core/src/http_retry.rs:16-27` and retry loop at `104-155`
- Impact: All sessions use the same deterministic exponential schedule. During a provider outage or rate-limit event, concurrent requests synchronize on the same retry times and can amplify load exactly when the upstream is degraded. This conflicts with the repository priority of predictable behavior under failures.
- Suggested fix: Add jitter to computed backoff, ideally full jitter or decorrelated jitter, while still honoring a bounded `Retry-After`. Make jitter injectable or deterministic in tests so retry behavior remains testable.

### Model cache refresh is sequential and fail-fast across providers

- Location: `crates/llm-providers/kraai-provider-core/src/lib.rs:405-410`, called after config load at `389`
- Impact: `update_models_list` awaits each provider's `cache_models()` serially. Slow provider discovery delays every later provider, and one failure aborts the whole config load even if other providers are usable. The OpenAI-compatible provider performs real HTTP discovery in `crates/llm-providers/kraai-provider-openai-chat-completions/src/provider.rs:86-124`, so this path can block startup on network latency.
- Suggested fix: Refresh providers concurrently with a bounded limit and collect per-provider errors. Preserve successfully loaded providers and surface partial failures through a structured result/event. If config load must fail on discovery errors, make that an explicit policy rather than an incidental consequence of serial `?`.

### `ProviderManager::load_config` replaces all providers after async work without preserving the previous good state on reload failure

- Location: `crates/llm-providers/kraai-provider-core/src/lib.rs:330-390`
- Impact: The method builds a new provider map and only assigns it at lines `384-387`, which avoids partial replacement. However, failures during validation, construction, model registration, or model caching return a generic error without a structured reload result. Runtime config reload callers cannot distinguish "old provider set is still active" from "manager is empty/unusable" without external knowledge.
- Suggested fix: Return a typed `ProviderLoadReport` that says whether the active set changed and includes per-provider/per-model failures. This also gives the runtime enough information to emit useful reload status instead of a single opaque error.

## Low Severity / Maintainability

### Core schema types duplicate validation rules across runtime and providers

- Location: schema types in `crates/llm-providers/kraai-provider-core/src/lib.rs:46-139`; runtime settings validation in `crates/kraai-runtime/src/settings.rs:181-263`; provider-specific field validation in `crates/llm-providers/kraai-provider-openai-codex/src/provider.rs:101-125`
- Impact: `FieldDefinition` declares field kind, requiredness, secret-ness, and defaults, but core does not provide a validator that enforces those declarations. As a result, every provider has to remember to duplicate type/required checks, and runtime settings has separate structural checks. This will drift as more providers and field kinds are added.
- Suggested fix: Add a core `validate_dynamic_config(definitions, config)` helper that checks required fields, unknown fields policy, type compatibility, and default handling. Provider-specific validators should only add semantic checks that cannot be expressed by field definitions.

### `FieldValueKind::SecretString` overlaps with `FieldDefinition.secret`

- Location: `crates/llm-providers/kraai-provider-core/src/lib.rs:103-120`
- Impact: Secrecy is represented both as a value kind (`SecretString`) and a boolean (`secret`). The two can disagree, leaving UI/persistence code to guess which one is authoritative. This is a data-model smell for settings redaction and serialization.
- Suggested fix: Keep one representation. Prefer `FieldValueKind::String` plus `secret: bool`, or remove `secret` and make secrecy part of the kind. Add validation that provider definitions cannot express contradictory secrecy metadata during factory registration.

### `DynamicValue` is too narrow for provider settings

- Location: `crates/llm-providers/kraai-provider-core/src/lib.rs:46-101`
- Impact: Provider configs can only express strings, bools, and signed integers. Field kinds already include `Url` as a string-like special case, but future settings such as floats, lists, headers, token budgets, model parameters, or structured provider options will require either string encoding or another breaking expansion of this enum.
- Suggested fix: Decide whether dynamic config is meant to be a small typed settings layer or general provider JSON/TOML config. If it is settings, add explicit missing scalar/list types with validation. If it is provider config, consider `serde_json::Value`/`toml::Value` internally plus typed accessors.

### `ProviderFactory::create` loses error type information

- Location: trait signature at `crates/llm-providers/kraai-provider-core/src/lib.rs:187-193`; wrapping at `209-214`
- Impact: Factory implementations return `color_eyre::Result`, but registration wraps all errors as `ProviderError::ConfigParseError(error.to_string())`. Construction failures, auth setup failures, and invalid config all collapse into the same stringly error. That makes caller behavior and tests less precise.
- Suggested fix: Change the trait to return `Result<Box<dyn Provider>, ProviderError>` or introduce explicit provider-core error variants such as `InvalidConfig`, `AuthUnavailable`, and `ProviderInitializationFailed`. Keep `color_eyre` at binary/runtime boundaries rather than in the core trait.

### `ProviderManager` exposes cloneable shared providers without lifecycle semantics

- Location: `ProviderManager` stores `Arc<dyn Provider>` at `crates/llm-providers/kraai-provider-core/src/lib.rs:141-144`; `get_provider` clones it at `322-324`; `Provider` has no shutdown/reset hooks at `453-476`
- Impact: Callers can hold old provider instances after a config reload, and providers that own HTTP clients, auth state, background caches, or future resources have no lifecycle hook. This is probably acceptable today, but it will become hard to reason about once providers do background refresh or per-provider concurrency limits.
- Suggested fix: Make provider handles explicit if long-lived references are intended, or restrict direct provider access and route operations through `ProviderManager`. Add optional lifecycle hooks if providers need cleanup or reload notifications.

### Unused `toml` dependency in provider-core

- Location: `crates/llm-providers/kraai-provider-core/Cargo.toml:20`
- Impact: `toml` is listed as a dependency but there are no `toml` references in this crate. It increases compile surface and makes ownership of config parsing less clear. Runtime settings owns TOML parsing in `crates/kraai-runtime/src/settings.rs:69-72` and `90-92`.
- Suggested fix: Remove the dependency from provider-core unless config parsing is intentionally being moved into this crate.

## Test Gaps To Add

- Invalid retry policy tests: zero attempts, capped maximum delay, and maximum elapsed retry budget once those invariants exist.
- Retry behavior tests with jitter using an injectable deterministic RNG/clock.
- `Retry-After` tests for future HTTP-date values and absurdly large values, especially once maximum delay caps are added.
- `ProviderManager::load_config` tests for duplicate provider ids, duplicate model ids, model registration failure preserving old providers, and model cache failure behavior.
- Core config-schema validation tests once `FieldDefinition` is made authoritative.
- Concurrency tests for model cache refresh so one slow provider does not block unrelated providers.
