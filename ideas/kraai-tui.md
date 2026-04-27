# kraai-tui review notes

## High: chat rendering can hang forever on narrow terminal widths

- **Where:** [crates/kraai-tui/src/components/chat_history.rs](/home/ominit/code/kraai/crates/kraai-tui/src/components/chat_history.rs:96), especially `wrap_with_prefix` lines 128-141 and `push_wrapped_spans` lines 274-305.
- **Problem:** Both wrapping loops can make no progress when the available content width is zero after subtracting the prefix width. `wrap_with_prefix` pushes a fitted prefix and `continue`s without draining `chars`; `push_wrapped_spans` pushes the prefix and `continue`s without advancing `idx` when `total > 0`.
- **Impact:** A very narrow terminal, popup, or future layout that calls these helpers with `width <= prefix_width` can spin the UI thread at 100% CPU and make the TUI unrecoverable. This is reachable today through assistant markdown/list/quote rendering and user/tool message rendering when `content_width` is small.
- **Suggested fix:** Make zero-available-width branches produce one prefix-only line and then break or advance by at least one character. Add regression tests for widths smaller than `"tool: "`, `"│ "`, bullet prefixes, and ordered-list prefixes.

## Medium: session menu selection can point past the end after refresh/delete

- **Where:** [crates/kraai-tui/src/app/runtime_handlers.rs](/home/ominit/code/kraai/crates/kraai-tui/src/app/runtime_handlers.rs:541) and [crates/kraai-tui/src/app/terminal.rs](/home/ominit/code/kraai/crates/kraai-tui/src/app/terminal.rs:393).
- **Problem:** `RuntimeResponse::Sessions` clamps only when `sessions_menu_index > sessions.len()`. The valid range is `0..=sessions.len()` because index `0` is "Start new chat" and session `n` is at menu index `n`. That part is technically okay, but deletion only `retain`s the removed session and does not reclamp immediately. A stale selection can remain on a now-missing row until a later sessions response arrives, so Enter/x becomes a no-op while the UI still has a selected line model.
- **Impact:** The sessions menu can appear unresponsive after deleting the last selected session or after a background refresh shrinks the list. It is a state consistency issue and easy to miss because the menu uses saturating indexing instead of panicking.
- **Suggested fix:** Centralize session menu clamping in a helper, call it after every mutation of `state.sessions`, and add tests for deleting the final session and receiving a shorter sessions list while `sessions_menu_index` is at the old end.

## Medium: terminal raw-mode cleanup is not exception-safe around setup failures

- **Where:** [crates/kraai-tui/src/main.rs](/home/ominit/code/kraai/crates/kraai-tui/src/main.rs:100).
- **Problem:** `ratatui::init()` runs before the `execute!(EnableMouseCapture, EnableBracketedPaste, PushKeyboardEnhancementFlags(...))?` call. If that `execute!` fails, `main` returns early and skips `ratatui::restore()`. A panic inside `app.run` would also skip the explicit cleanup path.
- **Impact:** Users can be left with a broken terminal state after startup failures or panics. For a TUI, cleanup reliability is part of correctness.
- **Suggested fix:** Introduce a small terminal guard type that performs all setup and restores in `Drop`, or use an existing ratatui/crossterm guard pattern. Tests can cover the guard behavior with an injectable writer/setup function, while manual smoke tests should cover normal startup and error paths.

## Medium: runtime bridge serializes every async request through one blocking worker

- **Where:** [crates/kraai-tui/src/app/runtime_bridge.rs](/home/ominit/code/kraai/crates/kraai-tui/src/app/runtime_bridge.rs:39).
- **Problem:** `spawn_runtime_bridge` receives requests on one thread and `block_on`s each runtime call sequentially. Long calls such as model discovery, settings save, auth, history reads, or session operations block unrelated quick requests behind them.
- **Impact:** Under slow provider/network/filesystem behavior, the TUI can show stale state and delayed approvals/cancel responses even though the UI loop itself is separate. This conflicts with the project priority of predictable behavior during failures and partial streams.
- **Suggested fix:** Move to a dedicated async task/actor that can classify requests by ordering requirements. Keep mutations that must be ordered serialized, but allow independent reads/status requests to run concurrently with bounded concurrency and stale-response filtering. Add tests around slow requests not delaying cancellation/status updates.

## Medium: render cache line counts saturate at `u16`, so very long chats lose scroll range

- **Where:** [crates/kraai-tui/src/app/state.rs](/home/ominit/code/kraai/crates/kraai-tui/src/app/state.rs:244) and [crates/kraai-tui/src/components/chat_history.rs](/home/ominit/code/kraai/crates/kraai-tui/src/components/chat_history.rs:839).
- **Problem:** `ChatRenderCache.total_lines`, `scroll`, and scroll calculations are `u16`. `refresh_chat_render_cache` uses saturating addition, so histories above 65,535 rendered lines collapse to the same max scroll and cannot address older/newer lines accurately.
- **Impact:** Long-running LLM sessions with verbose outputs can make chat navigation incorrect. This is especially plausible for agent logs, tool output, and repeated sessions.
- **Suggested fix:** Store render counts and scroll offsets as `usize` internally, converting to `u16` only at terminal coordinate boundaries. Add a unit test that builds or fakes more than `u16::MAX` rendered lines and verifies max scroll remains accurate.

## Low: input/editing helpers duplicate wrapping and cursor logic

- **Where:** [crates/kraai-tui/src/components/text_input.rs](/home/ominit/code/kraai/crates/kraai-tui/src/components/text_input.rs:34) and [crates/kraai-tui/src/components/chat_history.rs](/home/ominit/code/kraai/crates/kraai-tui/src/components/chat_history.rs:96).
- **Problem:** `TextInput` and `ChatHistory` both implement character wrapping, prefix handling, and width edge cases independently. `TextInput` handles `available == 0` safely; `ChatHistory` does not, which is a good example of the duplication producing divergent behavior.
- **Impact:** More wrapping bugs are likely as markdown, selection, cursor movement, and rendering evolve. The code is also harder to audit for Unicode and narrow-width correctness.
- **Suggested fix:** Extract a small shared wrapping module that returns structured segments with source byte ranges and display text. Use it for text input, chat rendering, and selection hit-testing.

## Low: tests are broad but miss terminal-width and failure-path regressions

- **Where:** [crates/kraai-tui/src/components/chat_history.rs](/home/ominit/code/kraai/crates/kraai-tui/src/components/chat_history.rs:883) and [crates/kraai-tui/src/app/tests.rs](/home/ominit/code/kraai/crates/kraai-tui/src/app/tests.rs:429).
- **Problem:** There are many useful interaction/render tests, but the current suite does not cover the narrow-width infinite-loop paths, terminal setup cleanup, or session index shrink/delete cases.
- **Impact:** The highest-risk TUI edge cases are not protected, so future refactors can reintroduce hangs or stale UI states without detection.
- **Suggested fix:** Add targeted regression tests first, then refactor. For the infinite-loop cases, avoid tests that can hang indefinitely by exposing pure helpers with deterministic output or adding a progress invariant in the wrapper implementation.
