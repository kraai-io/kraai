# toon-schema

A proc-macro derive crate for generating Toon format schema documentation from Rust structs.

## Overview

`toon-schema` automatically generates structured schema documentation for your Rust types, making it easy to document APIs and data structures in the Toon format. It provides compile-time validation of examples and ensures your documentation stays in sync with your code.

## Features

- **Automatic schema generation** from Rust struct definitions
- **Compile-time validation** of JSON examples
- **Serde integration** - respects `#[serde(rename)]`, `#[serde(skip)]`, and `#[serde(default)]`
- **Custom ranges** for Vec fields (`min`/`max` attributes)
- **Type safety** - errors on unsupported types instead of silently converting

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
    println!("{}", ReadFileArgs::toon_schema());
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

### Custom Ranges

For Vec fields, you can specify minimum and maximum counts:

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

- `name = "..."` - Custom tool name (defaults to struct name)
- `description = "..."` - Tool description

### Field-level (`#[toon_schema(...)]`)

- `description = "..."` - Field description
- `example = "..."` - **Required.** JSON example for the field
- `min = N` - Minimum count for Vec fields
- `max = N` - Maximum count for Vec fields

## Error Messages

The crate provides helpful compile-time error messages:

- **Missing example**: Suggests appropriate example for the type
- **Invalid JSON**: Shows what's wrong and how to fix it
- **Unknown type**: Lists supported types
- **Custom range on non-Vec**: Explains ranges only work with Vec types
- **Reserved field name**: 'tool' is reserved in Toon format

## License

MIT OR Apache-2.0