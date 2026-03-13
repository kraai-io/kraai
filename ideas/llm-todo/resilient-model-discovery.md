# Resilient Model Discovery

## Why this matters

Provider refresh should degrade gracefully. Right now one bad provider refresh can poison global model loading, and refreshing can temporarily erase otherwise usable cached models.

## Current gap

`ProviderManager::load_config()` and `update_models_list()` are effectively all-or-nothing. The OpenAI-compatible provider also clears cached models before rebuilding them, and configured-only models can disappear if the remote discovery endpoint does not return them.

## Goal

Make model discovery incremental, failure-tolerant, and explicit about provider health.

## Plan

1. Add per-provider refresh results with states such as `Healthy`, `Stale`, and `Failed`.
2. Keep last-known-good model caches when refresh fails.
3. Merge discovered models with explicitly configured models instead of replacing them.
4. Surface per-provider refresh errors back to runtime callers.
5. Keep successful providers usable even if one provider fails validation or discovery.
6. Add retry/backoff hooks later if needed, but keep the first version synchronous and deterministic.

## Milestones

1. Provider health result type.
2. Last-known-good cache retention.
3. Merge configured and discovered models.
4. Runtime exposure of refresh health.

## Validation

1. Tests with one healthy provider and one failing provider.
2. Tests for configured-only models.
3. Tests proving stale caches remain available after refresh failures.

## Risks

Stale model lists must be visible as stale. Do not silently present degraded state as if it were freshly loaded.
