# Robust SSE Streaming

## Why this matters

Streaming is central to responsiveness, but the current SSE path is minimal and text-focused. That makes it easy to miss finish signals, structured deltas, or transport edge cases.

## Current gap

The SSE implementation mostly extracts `data:` payloads and the provider then keeps only `delta.content`. Comments, event types, terminal markers, empty deltas, and some transport failures are not modeled explicitly.

## Goal

Turn SSE handling into a first-class streaming layer with explicit end-of-stream and error semantics.

## Plan

1. Replace the raw `data` extraction helper with an `SseEvent` parser that preserves:
   - `event`
   - `data`
   - comment lines
   - terminal state
2. Handle multi-line `data:` blocks correctly.
3. Preserve `[DONE]` and finish markers as semantic events instead of letting them disappear into filtering.
4. Classify transport errors, parse errors, and provider-declared errors separately.
5. Make runtime stream handling react differently to:
   - clean completion
   - provider error chunk
   - malformed stream
   - cancelled stream
6. Add focused tests for partial UTF-8 boundaries, missing trailing blank lines, and mid-stream disconnects.

## Milestones

1. Standalone SSE parser with unit tests.
2. Provider migration to the new parser.
3. Runtime error mapping cleanup.

## Validation

1. Unit tests for SSE framing edge cases.
2. Integration tests for provider streams that emit usage or finish metadata without text.
3. Cancellation tests proving the runtime never double-completes a message.

## Risks

Streaming bugs are usually race bugs. Keep the parser deterministic and keep cancellation logic separate from transport parsing.
