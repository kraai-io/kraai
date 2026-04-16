# kraai-toon-schema

`kraai-toon-schema` is a proc-macro crate for generating TOON tool schemas at compile time.

## API

Use `toon_tool!` to declare:
- the tool name
- the tool description
- the owned type definitions used by the tool
- the root argument type
- object-style examples

The macro emits:
- the declared Rust structs
- `tool_name()`
- `toon_schema()`

Examples are rendered with `toon-format::encode_default`, so the generated TOON matches the canonical encoder exactly.

## Example

```rust
use kraai_toon_schema::toon_tool;

toon_tool! {
    name: "read_files",
    description: "Read files from the filesystem",
    types: {
        #[derive(serde::Deserialize, serde::Serialize)]
        struct ReadFileArgs {
            #[toon_schema(description = "File paths to read", min = 1)]
            files: Vec<String>,

            #[toon_schema(description = "Optional encoding")]
            encoding: Option<String>,
        }
    },
    root: ReadFileArgs,
    examples: [
        {
            files: ["/etc/passwd", "/etc/hosts"],
            encoding: "utf-8"
        }
    ]
}
```

Generated schema shape:

```text
# Read files from the filesystem
tool: read_files
# File paths to read
files[1:]: array<string>
# Optional encoding
encoding[0:1]: string

Examples:
<tool_call>
tool: read_files
files[2]: /etc/passwd,/etc/hosts
encoding: utf-8
</tool_call>
```

## Supported type shapes

- named structs declared inside `types:`
- nested named structs
- `Option<T>`
- `Vec<T>`
- fixed arrays `[T; N]`
- `HashMap<String, T>` and `BTreeMap<String, T>`

## Supported field attributes

- `#[toon_schema(description = "...")]`
- `#[toon_schema(min = N)]`
- `#[toon_schema(max = N)]`
- `#[serde(rename = "...")]`
- `#[serde(skip)]`
- `#[serde(default)]`

## Not supported

- enums
- tuple structs
- unit structs
- external nested types not declared inside `types:`
- arbitrary custom serde serialization behavior
