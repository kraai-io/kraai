# toon-schema

A proc-macro derive crate for generating Toon format schema documentation from Rust structs.

## Overview

`toon-schema` automatically generates structured schema documentation for your Rust types at **compile time**. It provides compile-time validation of complete tool-call examples and type checking, ensuring your documentation stays in sync with your code.

## Features

- **Fully compile-time** - Schema generation, Toon encoding, and type validation all happen during macro expansion
- **Zero runtime dependencies** - No external crates needed at runtime; returns `&'static str` directly
- **Compile-time validation** - Full JSON tool-call examples validated and type-checked during compilation
- **Serde integration** - Respects `#[serde(rename)]`, `#[serde(skip)]`, and `#[serde(default)]`
- **Custom ranges** for Vec fields (`min`/`max` attributes)
- **Helpful error messages** - Clear compile-time errors with suggestions

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
toon-schema = "0.1.0"
serde = { version = "1.0", features = ["derive"] }
```

### Basic Example

```rust
use serde::{Deserialize, Serialize};
use toon_schema::ToonSchema;

#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Read files from the filesystem",
    example = r#"{"files":["/etc/passwd"],"max_lines":100}"#,
    example = r#"{"files":["/etc/passwd","/etc/hosts"]}"#
)]
struct ReadFileArgs {
    #[toon_schema(description = "File paths to read")]
    files: Vec<String>,

    #[toon_schema(description = "Maximum number of lines to read")]
    max_lines: Option<i32>,
}

fn main() {
    // Returns &'static str - fully computed at compile time
    let schema: &'static str = ReadFileArgs::toon_schema();
    println!("{}", schema);
    
    // Also available: tool name
    println!("Tool: {}", ReadFileArgs::tool_name());
}
```

Output:
```
# Read files from the filesystem
tool: ReadFileArgs
# File paths to read
files[0:]: array<string>
# Maximum number of lines to read
max_lines[0:1]: integer

Examples:
<tool_call>
tool: ReadFileArgs
files[1]: /etc/passwd
max_lines: 100
</tool_call>

<tool_call>
tool: ReadFileArgs
files[2]: /etc/passwd,/etc/hosts
</tool_call>
```

### Custom Tool Name

Use the `name` attribute to customize the tool name:

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(
    description = "Greeting tool",
    name = "say_hello",
    example = r#"{"name":"World"}"#
)]
struct GreetingArgs {
    #[toon_schema(description = "Name to greet")]
    name: String,
}

// GreetingArgs::tool_name() returns "say_hello"
```

### Custom Ranges

For Vec fields, specify minimum and maximum counts:

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"items":[1,2,3]}"#)]
struct BatchRequest {
    #[toon_schema(description = "Items to process", min = 1, max = 100)]
    items: Vec<i32>,
}
```

This generates `items[1:100]: array<integer>`.

### Serde Integration

The derive respects serde attributes:

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"api_key":"secret123","timeout":30}"#)]
struct ApiRequest {
    #[serde(rename = "api_key")]
    #[toon_schema(description = "API authentication key")]
    key: String,
    
    #[serde(skip)]
    internal_id: String, // Not included in schema
    
    #[serde(default)]
    #[toon_schema(description = "Timeout in seconds")]
    timeout: i32, // Shows default in schema
}
```

## Supported Types

- **Primitives**: `String`, `i8`-`i128`, `u8`-`u128`, `isize`, `usize`, `f32`, `f64`, `bool`
- **Collections**: `Vec<T>` (arrays), `Option<T>` (optional fields)

**Not supported**: Enums, nested structs, maps, tuples, or generic types beyond `Vec` and `Option`.

## Attributes

### Struct-level (`#[toon_schema(...)]`)

| Attribute | Description |
|-----------|-------------|
| `name = "..."` | Custom tool name (defaults to struct name) |
| `description = "..."` | Tool description |
| `example = "..."` | **Required, repeatable.** Full JSON object example for the tool |

### Field-level (`#[toon_schema(...)]`)

| Attribute | Description |
|-----------|-------------|
| `description = "..."` | Field description |
| `min = N` | Minimum count for Vec fields |
| `max = N` | Maximum count for Vec fields |

## Compile-Time Validation

All validation happens at compile time:

- **Missing example**: Suggests a struct-level JSON object example
- **Invalid JSON**: Shows what's wrong and how to fix it
- **Type mismatch**: Example value doesn't match field type
- **Unknown field**: Example contains a key not declared in the schema
- **Unknown type**: Lists supported types
- **Custom range on non-Vec**: Explains ranges only work with Vec types
- **Reserved field name**: 'tool' is reserved in Toon format

### Example: Type Mismatch Error

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(example = r#"{"count":"not a number"}"#)]
struct BadExample {
    #[toon_schema(description = "Count")]
    count: i32,
}
```

Produces a compile-time error:
```
error: example 1 field 'count' has wrong type; expected integer, got string
```

## Generated Methods

The derive generates two methods:

```rust
impl MyStruct {
    /// Returns the tool name (from `name` attribute or struct name)
    pub fn tool_name() -> &'static str;
    
    /// Returns the complete Toon format schema with examples
    pub fn toon_schema() -> &'static str;
}
```

Both methods return `&'static str` with all computation done at compile time.

## License

MIT OR Apache-2.0
