# Structured Transcript And Context Compiler

## Why this matters

The project is heavily LLM-driven, but core transcript storage still collapses conversation history into plain role/content strings. That limits tool semantics, provider portability, token optimization, and safe continuation after tool use.

## Current gap

`types::ChatMessage` and `types::Message` flatten data into text content, and tool results are rendered back into human-formatted strings. Start and continuation paths rebuild provider messages by replaying full ancestry every time, with no token budgeting or provider-specific compilation step.

## Goal

Introduce a structured transcript model and a provider-facing context compiler that can produce wire messages deliberately instead of replaying raw strings.

## Plan

1. Define transcript item variants such as:
   - `SystemPrompt`
   - `UserMessage`
   - `AssistantMessage`
   - `ToolCall`
   - `ToolResult`
   - `InternalAnnotation`
2. Persist structured transcript items without breaking current message history reads.
3. Add a `ContextCompiler` layer that takes transcript items plus provider capabilities and emits provider wire messages.
4. Put provider-specific ordering, truncation, and serialization decisions in the compiler instead of inside `AgentManager`.
5. Add token budget hooks so the compiler can later support summaries, truncation, or reuse of cached compiled prefixes.
6. Migrate the OpenAI chat-completions provider to consume compiler output instead of a raw `Vec<ChatMessage>`.

## Milestones

1. Introduce transcript item types and adapters from existing messages.
2. Build a compiler that reproduces current behavior exactly.
3. Add structured tool-call and tool-result items.
4. Add token budget and summary boundaries.

## Validation

1. Golden tests for current sessions compiling to the same provider payloads as before.
2. Tests covering tool continuation, cancelled turns, and long histories.
3. Benchmarks comparing full replay vs cached compiled context for repeated continuations.

## Risks

This is a foundational change. The first version should preserve current text-based messages and add structure in parallel rather than replacing persistence in one shot.
