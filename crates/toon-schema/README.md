# toon-schema

A proc-macro derive crate for generating Toon format schema documentation from Rust structs.

## Overview

`toon-schema` automatically generates structured schema documentation for your Rust types at **compile time**. It provides compile-time validation of examples and type checking, ensuring your documentation stays in sync with your code.

## Features

- **Fully compile-time** - Schema generation, Toon encoding, and type validation all happen during macro expansion
- **Zero runtime dependencies** - No external crates needed at runtime; returns `&'static str` directly
- **Compile-time validation** - JSON examples validated and type-checked during compilation
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
#[toon_schema(description = "Read files from the filesystem")]
struct ReadFileArgs {
    #[toon_schema(
        description = "File paths to read",
        example = "[\"/etc/passwd\", \"/etc/hosts\"]"
    )]
    files: Vec<String>,
    
    #[toon_schema(
        description = "Maximum number of lines to read",
        example = "100"
    )]
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

Example:
tool: ReadFileArgs
files[2]: /etc/passwd,/etc/hosts
max_lines: 100
```

### Custom Tool Name

Use the `name` attribute to customize the tool name:

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
#[toon_schema(description = "Greeting tool", name = "say_hello")]
struct GreetingArgs {
    #[toon_schema(description = "Name to greet", example = "\"World\"")]
    name: String,
}

// GreetingArgs::tool_name() returns "say_hello"
```

### Custom Ranges

For Vec fields, specify minimum and maximum counts:

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
struct BatchRequest {
    #[toon_schema(
        description = "Items to process",
        example = "[1, 2, 3]",
        min = 1,
        max = 100
    )]
    items: Vec<i32>,
}
```

This generates `items[1:100]: array<integer>`.

### Serde Integration

The derive respects serde attributes:

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
struct ApiRequest {
    #[serde(rename = "api_key")]
    #[toon_schema(description = "API authentication key", example = "\"secret123\"")]
    key: String,
    
    #[serde(skip)]
    internal_id: String, // Not included in schema
    
    #[serde(default)]
    #[toon_schema(description = "Timeout in seconds", example = "30")]
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

### Field-level (`#[toon_schema(...)]`)

| Attribute | Description |
|-----------|-------------|
| `description = "..."` | Field description |
| `example = "..."` | **Required.** JSON example for the field |
| `min = N` | Minimum count for Vec fields |
| `max = N` | Maximum count for Vec fields |

## Compile-Time Validation

All validation happens at compile time:

- **Missing example**: Suggests appropriate example for the type
- **Invalid JSON**: Shows what's wrong and how to fix it
- **Type mismatch**: Example value doesn't match field type
- **Unknown type**: Lists supported types
- **Custom range on non-Vec**: Explains ranges only work with Vec types
- **Reserved field name**: 'tool' is reserved in Toon format

### Example: Type Mismatch Error

```rust
#[derive(ToonSchema, Serialize, Deserialize)]
struct BadExample {
    #[toon_schema(example = "\"not a number\"")] // Wrong! Should be a number
    count: i32,
}
```

Produces a compile-time error:
```
error: Type mismatch for field 'count': expected Integer, got JSON value '"not a number"'
```

## Generated Methods

The derive generates two methods:

```rust
impl MyStruct {
    /// Returns the tool name (from `name` attribute or struct name)
    pub fn tool_name() -> &'static str;
    
    /// Returns the complete Toon format schema with example
    pub fn toon_schema() -> &'static str;
}
```

Both methods return `&'static str` with all computation done at compile time.

## License

MIT OR Apache-2.0
