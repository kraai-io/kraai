#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

mod ir;
mod parse;

/// Build a full TOON tool schema at compile time.
///
/// The macro owns the tool's type definitions, metadata, and examples so it can
/// validate nested example shapes and emit a final `&'static str` schema
/// without any runtime assembly.
#[proc_macro]
pub fn toon_tool(input: TokenStream) -> TokenStream {
    let schema = parse_macro_input!(input with parse::parse_tool_schema);

    match expand_tool(schema) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn expand_tool(schema: ir::ToolSchema) -> syn::Result<proc_macro2::TokenStream> {
    let rendered = parse::render_schema(&schema)?;
    let root = syn::Ident::new(&schema.root, proc_macro2::Span::call_site());
    let name = &schema.name;
    let type_items = schema.types.iter().map(|item| &item.item);

    Ok(quote! {
        #(#type_items)*

        impl #root {
            pub fn tool_name() -> &'static str {
                #name
            }

            pub fn toon_schema() -> &'static str {
                #rendered
            }
        }
    })
}
