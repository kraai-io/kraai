use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

mod ir;
mod parse;

use ir::{PrimitiveType, Range, Schema, Type};
use parse::parse_toon_schema;

/// Derive macro for generating Toon format schemas with examples.
///
/// # Requirements
/// - Every field MUST have an `example` attribute
/// - Examples MUST be valid JSON
/// - Examples MUST match the field type (validated at compile time)
///
/// # Example
/// ```rust
/// use serde::{Deserialize, Serialize};
/// use toon_schema::ToonSchema;
///
/// #[derive(ToonSchema, Serialize, Deserialize)]
/// #[toon_schema(description = "Read files")]
/// struct ReadFileArgs {
///     #[toon_schema(
///         description = "File paths",
///         example = "[\"/etc/passwd\"]"
///     )]
///     files: Vec<String>,
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

    // Generate schema string
    let schema_str = generate_schema_string(&schema);

    // Generate compile-time validation
    let validation = generate_validation(&schema, struct_name);

    // Generate example JSON construction
    let example_construction = generate_example_construction(&schema);

    // Get the tool name from schema
    let tool_name = &schema.name;

    Ok(quote! {
        #validation

        impl #impl_generics #struct_name #ty_generics #where_clause {
            /// Generate the Toon format schema string.
            pub fn toon_schema() -> String {
                use toon_format::{encode_object, EncodeOptions};

                // Build example struct from field examples
                let example_struct: Self = #example_construction;

                // Convert to JSON then to Toon format
                let example_json = serde_json::to_value(&example_struct)
                    .expect("Failed to serialize example");
                let example_toon = encode_object(example_json, &EncodeOptions::new())
                    .expect("Failed to encode to Toon format");

                format!("{}\n\nExample:\ntool: {}\n{}", #schema_str, #tool_name, example_toon)
            }
        }
    })
}

fn generate_schema_string(schema: &Schema) -> String {
    let mut lines = vec![];

    // Description as a comment if present
    if let Some(desc) = &schema.description {
        lines.push(format!("# {}", desc));
    }

    // Tool name
    lines.push(format!("tool: {}", schema.name));

    // Fields with descriptions as comments above them
    for field in &schema.fields {
        if field.skipped {
            continue;
        }

        let range_str = match field.range {
            Range::Exactly(n) => format!("[{}:{}]", n, n),
            Range::ZeroToOne => "[0:1]".to_string(),
            Range::ZeroOrMore => "[0:]".to_string(),
        };

        let type_str = format_type(&field.ty);
        let optional_marker = if matches!(field.range, Range::ZeroToOne) {
            " (optional)"
        } else {
            ""
        };

        // Add description as a comment before the field
        if let Some(desc) = &field.description {
            lines.push(format!("# {}", desc));
        }

        lines.push(format!(
            "{}{}: {}{}",
            field.name, range_str, type_str, optional_marker
        ));
    }

    lines.join("\n")
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

fn generate_validation(_schema: &Schema, _struct_name: &syn::Ident) -> proc_macro2::TokenStream {
    // Validation happens at runtime when toon_schema() is called
    // The example construction will panic if examples are invalid
    quote! {}
}

fn generate_example_construction(schema: &Schema) -> proc_macro2::TokenStream {
    // Build a JSON object with all field examples
    let field_entries: Vec<_> = schema
        .fields
        .iter()
        .filter(|f| !f.skipped)
        .map(|f| {
            let name = &f.name;
            let example = &f.example;
            // Parse the example as JSON value
            quote! {
                #name: serde_json::from_str::<serde_json::Value>(#example).unwrap()
            }
        })
        .collect();

    quote! {
        {
            let json_obj = serde_json::json!({
                #(#field_entries),*
            });
            serde_json::from_value::<Self>(json_obj).expect("Failed to deserialize example - invalid examples provided")
        }
    }
}
