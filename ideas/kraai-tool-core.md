# kraai-tool-core Findings

## High Severity

### 1. `read_text_file` can read outside the workspace without enforcing its own assessment

- Location: `crates/tools/kraai-tool-core/src/lib.rs:398-401`
- Related call sites: `crates/tools/kraai-tool-read-file/src/lib.rs:81-87`, `crates/tools/kraai-tool-open-file/src/lib.rs:64-82`
- Impact: `read_text_file` resolves the path and then directly calls `read_text_path`, but it does not reject `ResolvedToolPath::is_within_workspace() == false`. The permission model relies on `assess` having run and the caller respecting it. That makes the primitive easy to misuse and makes security depend on every future caller remembering to assess first. Because `read_files` and `open_file` both call this helper, any runtime bug that executes a prepared tool after stale approval or bypassed approval can read arbitrary absolute paths.
- Suggested fix: split the API into explicit variants, for example `read_workspace_text_file(...) -> Result<...>` that rejects outside-workspace paths and `read_any_text_file(...)` for tools that already have confirmed outside-workspace approval. Better: make execution receive an approval/permission token or assessed path produced by the guard, so the same resolved path and risk classification used for approval is also used for execution.

### 2. Workspace checks have a TOCTOU gap for symlink escapes

- Location: `crates/tools/kraai-tool-core/src/lib.rs:364-369`, `crates/tools/kraai-tool-core/src/lib.rs:398-412`
- Impact: `resolve_tool_path` canonicalizes for assessment, but execution later reopens the original path in `read_text_path`. A path can be assessed as inside the workspace, then swapped to a symlink before execution, causing a read or write outside the workspace while retaining the inside-workspace risk classification. The existing symlink test only covers a stable symlink that already exists at assessment time (`crates/tools/kraai-tool-core/src/lib.rs:637-659`), not a post-assessment swap.
- Suggested fix: carry a canonicalized execution path or verified handle from assessment/preparation into execution. For reads, open the file and then verify the opened target metadata/path before reading where the platform supports it. For writes, use canonical parent-directory checks immediately before creation/write and consider rejecting symlink components for workspace-scoped writes.

### 3. `normalize_tool_path` clamps excessive `..` at filesystem root, which can silently reclassify paths

- Location: `crates/tools/kraai-tool-core/src/lib.rs:344-355`
- Impact: Parent components are ignored whenever `normalized.parent().is_none()`. For absolute paths like `/../../etc/passwd`, this normalizes to `/etc/passwd` instead of preserving that the input tried to escape above root. For relative paths with a relative `workspace_root`, leading `..` can also be collapsed in surprising ways. This is risky in a security-sensitive helper because malformed paths become valid-looking paths.
- Suggested fix: require `workspace_root` to be absolute/canonical at construction time, reject paths that attempt to traverse above the starting root, and test absolute and relative edge cases. If clamping root is intentional, document it and keep this function out of security decisions.

## Medium Severity

### 4. Path state keys use display strings, so equivalent paths can miss read-freshness checks

- Location: `crates/tools/kraai-tool-core/src/lib.rs:479-496`
- Related state application: `crates/kraai-agent/src/tool_state.rs:169-190`
- Impact: file-read hashes are keyed by `path.display().to_string()`. The same file can be recorded as `/workspace/src/lib.rs`, `/workspace/./src/lib.rs`, or through a symlink/alternate spelling. `edit_file` uses `file_read_sha256` through this keying model, so freshness checks can fail open or fail closed depending on path spelling. It also makes persisted tool state less stable across platforms.
- Suggested fix: introduce a normalized `ToolPathKey` helper in `kraai-tool-core` and use it for all file read/open/close/edit state. Prefer canonical absolute paths when the target exists, with a well-defined normalized absolute path for not-yet-existing create targets.

### 5. Parser silently ignores unclosed `<tool_call>` blocks

- Location: `crates/tools/kraai-tool-core/src/toon_parser.rs:50-66`, `crates/tools/kraai-tool-core/src/toon_parser.rs:105-110`
- Impact: `extract_tool_call_blocks` only returns regex matches with closing `</tool_call>`. If the model emits `<tool_call>` and never closes it, the parser returns no success and no failure. That gives the model no corrective feedback and can make runtime behavior look like a normal text response rather than a malformed tool call.
- Suggested fix: detect unmatched opening tags and return a `ParseFailureKind::ToolCall` with the raw content and a specific "missing closing </tool_call>" error. Add tests for unclosed, nested, and stray closing tags.

### 6. Tool-call extraction is case-sensitive and exact-tag-only while thinking stripping is more permissive

- Location: `crates/tools/kraai-tool-core/src/toon_parser.rs:6-9`
- Impact: thinking tags are case-insensitive and allow attributes, but tool calls require exactly `<tool_call>` and `</tool_call>`. The stream guard also uses exact tags (`crates/kraai-runtime/src/runtime/tool_call_guard.rs:1-2`). If a provider emits `<tool_call id="...">` or different casing, the call is silently ignored unless it happens to create malformed visible text elsewhere. This is brittle for LLM-facing syntax.
- Suggested fix: either document a strict exact tag contract in the generated prompts and add negative tests, or make parser and stream guard share a single tolerant tag scanner. Avoid having parser and streaming guard implement subtly different grammars.

### 7. `ToolOutput` has no `Serialize`/`Debug` and uses an ambiguous untagged shape

- Location: `crates/tools/kraai-tool-core/src/lib.rs:36-46`
- Impact: `ToolOutput::Success` flattens arbitrary JSON data, while `ToolOutput::Error` is `{ "message": ... }`. A successful tool payload containing only `message` is indistinguishable from an error if deserialized back into `ToolOutput`, and the type cannot be directly serialized or debug-printed despite being the core output type. Current runtime likely converts output elsewhere, but the type design is easy to misuse.
- Suggested fix: derive `Debug`, `Clone`, and `Serialize`, and use an explicit tagged shape such as `{ "ok": true, "data": ... }` / `{ "ok": false, "error": ... }`, or keep `ToolOutput` internal and expose conversion into the persisted `serde_json::Value`.

### 8. Duplicate tool registration silently overwrites the previous tool

- Location: `crates/tools/kraai-tool-core/src/lib.rs:283-289`
- Impact: registering two tools with the same name replaces the first with no warning or error. In an agent framework where profiles and prompts depend on tool names, this can cause hard-to-debug behavior if a new tool collides with an existing name.
- Suggested fix: make `register_tool` return `Result<(), ToolError>` and add a `DuplicateTool(ToolId)` error. If overwrites are useful in tests, add an explicit `replace_tool` method.

## Low Severity / Maintainability

### 9. `TypedTool` requires `Clone` on both tool and args, forcing extra allocation for prepared calls

- Location: `crates/tools/kraai-tool-core/src/lib.rs:99-121`, `crates/tools/kraai-tool-core/src/lib.rs:191-193`
- Impact: prepared calls deserialize arguments once, but `call` receives `args.clone()`. Large argument payloads like file contents or many edit operations are copied before execution. This is probably fine today, but it makes heavy tool use more expensive and pushes all tool arg types toward cloneable owned data.
- Suggested fix: change `TypedTool::call` to take `&Self::Args`, or store args in an `Arc<Self::Args>` if owned async calls are required. Most current tool implementations only read args and would not need ownership.

### 10. Parser APIs allocate every block eagerly

- Location: `crates/tools/kraai-tool-core/src/toon_parser.rs:50-65`, `crates/tools/kraai-tool-core/src/toon_parser.rs:105-110`, `crates/tools/kraai-tool-core/src/toon_parser.rs:124-134`
- Impact: parsing builds a stripped visible string, then a `Vec<String>` of raw blocks, then clones each argument value while removing `tool`. For normal responses this is small, but large model outputs or many tool calls pay avoidable allocation costs.
- Suggested fix: iterate regex captures directly over the visible text and parse borrowed `&str` slices. Use object removal (`remove("tool")`) on the decoded map rather than cloning every non-tool field.

### 11. `format_text_with_line_numbers` drops the final empty line state

- Location: `crates/tools/kraai-tool-core/src/lib.rs:470-477`
- Impact: `str::lines()` omits the trailing empty item for terminal newlines. The test asserts `alpha\nbeta\n` renders as only two lines (`crates/tools/kraai-tool-core/src/lib.rs:607-611`). That may be intentional for compact output, but edit tools ask for exact line-ranged replacements, so hiding whether a file ends with a newline can cause model mistakes.
- Suggested fix: decide whether tool output should preserve final newline information. If yes, switch to a renderer that marks EOF newline state or includes a final blank numbered line. If no, document the lossy behavior in the read/open tool schemas.

### 12. `kraai-tool-core` contains both generic tool runtime abstractions and file-state utilities

- Location: `crates/tools/kraai-tool-core/src/lib.rs:23-25`, `crates/tools/kraai-tool-core/src/lib.rs:124-159`, `crates/tools/kraai-tool-core/src/lib.rs:335-497`
- Impact: the crate is still small, but `lib.rs` mixes tool registration, path normalization, file reads, file state deltas, and formatting. As more tools land, this file is likely to become a context-heavy catch-all.
- Suggested fix: split modules now while the surface is small: `manager.rs` for `ToolManager`/prepared calls, `path.rs` for path resolution, `file_state.rs` for read hashes and deltas, and `text.rs` for rendering. Keep re-exports in `lib.rs` for the public API.

## Test Gaps

- Add a regression test for unclosed `<tool_call>` returning a parse failure.
- Add path tests for `/../../x`, relative `workspace_root`, non-existent workspace roots, symlink parent directories that do not exist yet, and assessment/execution mismatch scenarios.
- Add tests for duplicate tool registration behavior.
- Add round-trip tests for `ToolOutput` once its serialized shape is clarified.
- Add tests proving file-read hashes use a stable canonical key for equivalent path spellings.
