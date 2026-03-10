#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

mod ir;
mod parse;
mod toon_encode;

use ir::{PrimitiveType, Schema, Type};
use parse::parse_toon_schema;
use toon_encode::encode_examples_toon;

/// Derive macro for generating Toon format schema documentation from Rust structs.
///
/// This macro generates all schema content at compile time, returning `&'static str`
/// with zero runtime overhead.
///
/// # Requirements
///
/// - Every schema MUST have at least one struct-level `example` attribute with valid JSON
/// - Examples MUST be complete JSON objects matching the schema (validated at compile time)
/// - Supported types: primitives, `Vec<T>`, `Option<T>`
///
/// # Generated Methods
///
/// - `tool_name() -> &'static str` - Returns the tool name
/// - `toon_schema() -> &'static str` - Returns the complete schema
///
/// # Example
///
/// ```rust
/// use serde::{Deserialize, Serialize};
/// use toon_schema::ToonSchema;
///
/// #[derive(ToonSchema, Serialize, Deserialize)]
/// #[toon_schema(
///     description = "Read files from the filesystem",
///     name = "read_file",
///     example = r#"{"files":["/etc/passwd"],"max_size":1048576}"#
/// )]
/// struct ReadFileArgs {
///     #[toon_schema(description = "File paths to read")]
///     files: Vec<String>,
///
///     #[toon_schema(description = "Maximum file size")]
///     max_size: i64,
/// }
///
/// // Both methods return &'static str - fully computed at compile time
/// let name: &'static str = ReadFileArgs::tool_name();  // "read_file"
/// let schema: &'static str = ReadFileArgs::toon_schema();
/// ```
///
/// # Attributes
///
/// ## Struct-level (`#[toon_schema(...)]`)
///
/// - `name = "..."` - Custom tool name (defaults to struct name)
/// - `description = "..."` - Tool description
/// - `example = "..."` - **Required.** Complete JSON object example for the tool. Repeatable.
///
/// ## Field-level (`#[toon_schema(...)]`)
///
/// - `description = "..."` - Field description
/// - `min = N` - Minimum count for Vec fields
/// - `max = N` - Maximum count for Vec fields
///
/// # Serde Integration
///
/// The derive respects `#[serde(rename)]`, `#[serde(skip)]`, and `#[serde(default)]`:
///
/// ```rust
/// use serde::{Deserialize, Serialize};
/// use toon_schema::ToonSchema;
///
/// #[derive(ToonSchema, Serialize, Deserialize)]
/// #[toon_schema(example = r#"{"api_key":"secret","timeout":30}"#)]
/// struct ApiRequest {
///     #[serde(rename = "api_key")]
///     #[toon_schema(description = "API key")]
///     key: String,  // Shows as "api_key" in schema
///
///     #[serde(skip)]
///     internal: String,  // Not included in schema
///
///     #[serde(default)]
///     #[toon_schema(description = "Timeout")]
///     timeout: i32,  // Shows "# default: default" in schema
/// }
/// ```
#[proc_macro_derive(ToonSchema, attributes(toon_schema, serde))]
pub fn derive_toon_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match impl_toon_schema(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn impl_toon_schema(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let schema = parse_toon_schema(&input)?;
    let struct_name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let tool_name = &schema.name;

    let complete_schema = generate_complete_schema(&schema)?;

    Ok(quote! {
        impl #impl_generics #struct_name #ty_generics #where_clause {
            /// Get the tool name for this schema.
            pub fn tool_name() -> &'static str {
                #tool_name
            }

            /// Generate the Toon format schema string.
            /// Fully generated at compile time.
            pub fn toon_schema() -> &'static str {
                #complete_schema
            }
        }
    })
}

fn generate_complete_schema(schema: &Schema) -> syn::Result<String> {
    let mut lines = vec![];

    if let Some(desc) = &schema.description {
        lines.push(format!("# {}", desc));
    }

    lines.push(format!("tool: {}", schema.name));

    for field in &schema.fields {
        if field.skipped {
            continue;
        }

        let range_str = field.range.format();
        let type_str = format_type(&field.ty);

        if let Some(desc) = &field.description {
            lines.push(format!("# {}", desc));
        }

        let mut field_line = format!("{}{}: {}", field.name, range_str, type_str);

        if let Some(default) = &field.default_value {
            field_line.push_str(&format!(" # default: {}", default));
        }

        lines.push(field_line);
    }

    lines.push(String::new());
    lines.push("Examples:".to_string());

    let examples = encode_examples_toon(schema)?;
    for (index, example_lines) in examples.iter().enumerate() {
        lines.push("<tool_call>".to_string());
        lines.extend(example_lines.iter().cloned());
        lines.push("</tool_call>".to_string());
        if index + 1 != examples.len() {
            lines.push(String::new());
        }
    }

    Ok(lines.join("\n"))
}

fn format_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => match p {
            PrimitiveType::String => "string".to_string(),
            PrimitiveType::Integer => "integer".to_string(),
            PrimitiveType::Float => "float".to_string(),
            PrimitiveType::Boolean => "boolean".to_string(),
        },
        Type::Array(inner) => format!("array<{}>", format_type(inner)),
    }
}
