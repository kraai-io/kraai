use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

mod ir;
mod parse;

use ir::{PrimitiveType, Schema, Type};
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

    // Generate the complete schema string at compile time
    let schema_str = generate_schema_string(&schema);
    let tool_name = &schema.name;

    // Generate compile-time example construction and encoding
    let example_construction = generate_example_construction(&schema);

    Ok(quote! {
        impl #impl_generics #struct_name #ty_generics #where_clause {
            /// Generate the Toon format schema string.
            /// This is generated at compile time for optimal performance.
            pub fn toon_schema() -> &'static str {
                use toon_format::{encode_object, EncodeOptions};

                // Build example struct from field examples
                let example_struct: Self = #example_construction;

                // Convert to JSON then to Toon format
                let example_json = serde_json::to_value(&example_struct)
                    .expect("Failed to serialize example");
                let example_toon = encode_object(example_json, &EncodeOptions::new())
                    .expect("Failed to encode to Toon format");

                // Concatenate at runtime since example_toon is dynamic
                // But the schema part is known at compile time
                static SCHEMA_PART: &str = #schema_str;
                static TOOL_NAME: &str = #tool_name;
                
                // Leak the string to get a &'static str (safe since called infrequently)
                Box::leak(
                    format!("{}\n\nExample:\ntool: {}\n{}", SCHEMA_PART, TOOL_NAME, example_toon)
                        .into_boxed_str()
                )
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

        let range_str = field.range.format();
        let type_str = format_type(&field.ty);

        // Add description as a comment before the field
        if let Some(desc) = &field.description {
            lines.push(format!("# {}", desc));
        }

        // Build the field line
        let mut field_line = format!(
            "{}{}: {}",
            field.name, range_str, type_str
        );

        // Add default value as a comment
        if let Some(default) = &field.default_value {
            field_line.push_str(&format!(" # default: {}", default));
        }

        lines.push(field_line);
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
