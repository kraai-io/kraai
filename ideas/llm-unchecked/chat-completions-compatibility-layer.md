# Chat Completions Compatibility Layer

## Why this matters

The provider layer currently handles a narrow text-only subset of chat completions. That limits correctness for tool calls, structured content, finish reasons, usage, and future provider integrations built on the same protocol family.

## Current gap

The OpenAI-compatible provider mostly treats messages as plain text and streaming as text deltas only. Tool messages, multipart content, refusal metadata, and non-content deltas are discarded.

## Goal

Build a typed compatibility layer for chat-completions style providers so the runtime can preserve structured behavior instead of flattening everything to text.

## Plan

1. Expand provider wire types to represent:
   - text content
   - tool calls
   - tool results
   - finish reasons
   - usage and refusal metadata where available
2. Add provider capability flags in `provider-core` so unsupported features fail clearly instead of degrading silently.
3. Make message normalization produce typed provider messages, not just role/content pairs.
4. Teach the provider to round-trip tool-result messages correctly.
5. Add contract tests for normal replies, tool-call replies, tool-result continuations, and mixed-content streams.

## Milestones

1. Add types and capability model.
2. Preserve finish reasons and usage.
3. Add tool-call and tool-result support.
4. Port future chat-completions providers onto the same layer.

## Validation

1. Golden tests against representative OpenAI-style payloads.
2. Tests proving unsupported capability use returns explicit errors.
3. Regression tests proving current plain-text flows still work.

## Risks

The mistake to avoid is provider-specific logic leaking back into `AgentManager`. Keep protocol translation inside the provider layer and transcript compiler.
