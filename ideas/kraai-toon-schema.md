# kraai-toon-schema findings

Scope: `crates/kraai-toon-schema`. Verification run: `cargo test -p kraai-toon-schema` passed.

## High: nested object shapes are not rendered in the schema

- References: `crates/kraai-toon-schema/src/parse.rs:801`, `crates/kraai-toon-schema/src/parse.rs:817`, `crates/kraai-toon-schema/src/parse.rs:857`, `crates/tools/kraai-tool-edit-file/src/lib.rs:23`, `crates/tools/kraai-tool-edit-file/src/lib.rs:46`
- Problem: `render_schema` only emits the root struct's fields. Nested structs are collapsed to `object` by `describe_type`, so schemas such as `edits[0:1]: array<object>` do not tell the model that an edit object requires `start_line`, `end_line`, `old_text`, and `new_text`.
- Impact: this is a token-efficiency and reliability issue. The macro validates nested examples, but the generated tool schema withholds the nested contract the model must follow. The current `edit_file` tool relies on examples to teach the nested shape, which is brittle when examples are sparse or when multiple object fields exist.
- Suggested fix: render named object definitions after the root field list, or inline object fields for nested structs. Prefer a compact deterministic section such as:
  - `EditOperation:`
  - `  start_line[1:1]: integer`
  - `  end_line[1:1]: integer`
  - `  old_text[1:1]: string`
  - `  new_text[1:1]: string`
- Tests to add: assert that `EditLike::toon_schema()` includes the nested `Edit` field names, not only `edits[0:]: array<object>`.

## High: the macro rejects forward references between declared structs

- References: `crates/kraai-toon-schema/src/parse.rs:117`, `crates/kraai-toon-schema/src/parse.rs:118`, `crates/kraai-toon-schema/src/parse.rs:412`
- Problem: `build_tool_schema` parses each struct using only `seen_names`, so a type can only refer to structs declared earlier in `types:`. This means `struct Parent { child: Child } struct Child { ... }` fails even though both types are owned by the macro.
- Impact: users must order declarations bottom-up. That is surprising, creates noisy diffs, and gets worse as tool schemas grow.
- Suggested fix: first collect and validate all struct names, then parse fields against the complete declared-name set. Keep duplicate-name detection in that first pass.
- Tests to add: a passing integration test where the root struct appears before a nested struct.

## Medium: invalid or misspelled attributes are silently ignored

- References: `crates/kraai-toon-schema/src/parse.rs:285`, `crates/kraai-toon-schema/src/parse.rs:291`, `crates/kraai-toon-schema/src/parse.rs:303`, `crates/kraai-toon-schema/src/parse.rs:316`, `crates/kraai-toon-schema/src/parse.rs:309`
- Problem: `#[toon_schema(min = ...)]` and `max` use `parse_u32_expr`, which returns `None` for unsupported expressions. Unknown `toon_schema` keys also fall through `_ => {}`. A typo like `minimum = 1`, or a bad literal like `min = "1"`, compiles and changes the generated schema.
- Impact: schema bugs become silent runtime/model behavior bugs instead of compile-time failures.
- Suggested fix: make `parse_toon_field_attr` reject unknown keys and reject non-integer `min`/`max` values with a span on the offending expression. Also detect duplicate attributes such as two descriptions or two mins.
- Tests to add: compile-fail cases for `#[toon_schema(min = "1")]` and `#[toon_schema(descripton = "...")]`.

## Medium: `min > max` produces an impossible range instead of a direct diagnostic

- References: `crates/kraai-toon-schema/src/parse.rs:343`, `crates/kraai-toon-schema/src/parse.rs:348`, `crates/kraai-toon-schema/src/ir.rs:89`
- Problem: `Vec<T>` ranges are constructed without validating `min <= max`. `#[toon_schema(min = 5, max = 3)]` renders as `[5:3]` and any provided example will fail length validation indirectly.
- Impact: the user gets a confusing error, or an impossible schema if validation does not exercise the field in a useful way.
- Suggested fix: validate bounded ranges at construction time and emit a clear compile error on the field: `min must be less than or equal to max`.
- Tests to add: compile-fail case for `min = 2, max = 1`.

## Medium: supported serde behavior is narrower than the docs imply

- References: `crates/kraai-toon-schema/README.md:81`, `crates/kraai-toon-schema/README.md:83`, `crates/kraai-toon-schema/src/parse.rs:245`, `crates/kraai-toon-schema/src/parse.rs:263`, `crates/kraai-toon-schema/src/parse.rs:269`
- Problem: only field-level `rename`, `skip`, and `default` are partially recognized. Common serde attributes such as struct-level `rename_all`, `skip_deserializing`, `skip_serializing`, `flatten`, `alias`, `default = "path"`, and `skip_serializing_if` are ignored or rendered only as comments.
- Impact: the schema can diverge from actual deserialization. In this crate that is especially risky because the schema is the model-facing contract for tool calls.
- Suggested fix: either fail fast on unsupported serde attributes inside `toon_tool!` or implement the subset needed by the tool crates. At minimum, parse struct-level `rename_all` and field-level `skip_deserializing` because they directly affect accepted input keys.
- Tests to add: one compile-fail test for unsupported serde behavior and one passing test for whichever serde subset is intentionally supported.

## Low: negative numeric examples are unsupported despite signed numeric field types

- References: `crates/kraai-toon-schema/src/parse.rs:568`, `crates/kraai-toon-schema/src/parse.rs:573`, `crates/kraai-toon-schema/src/parse.rs:597`, `crates/kraai-toon-schema/src/parse.rs:409`
- Problem: example values are parsed as `LitInt`/`LitFloat`. In Rust token syntax, `-1` is a unary expression, not a literal token, so a signed integer or float field cannot use a negative example even though signed numeric types are supported.
- Impact: limits examples for legitimate signed fields and makes the example DSL diverge from normal Rust expression syntax.
- Suggested fix: parse example values as `syn::Expr` for scalar cases and explicitly support unary minus for integer and float literals.
- Tests to add: a passing test with `{ delta: -1, ratio: -0.5 }`.

## Low: test coverage is mostly substring-based and misses full schema regressions

- References: `crates/kraai-toon-schema/tests/basic_types.rs:28`, `crates/kraai-toon-schema/tests/comprehensive.rs:38`, `crates/kraai-toon-schema/tests/new_features.rs:30`
- Problem: several tests use `contains` checks for selected fragments. They do not lock down field order, blank lines, default formatting, nested definitions, or the exact root schema shape.
- Impact: schema format regressions can slip through while still satisfying a few substrings.
- Suggested fix: add snapshot-style exact string assertions for representative schemas. Keep the `toon_format` exact-match tests for examples, but add exact tests for the schema header and field-definition sections.

