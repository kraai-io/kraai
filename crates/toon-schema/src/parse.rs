use crate::ir::{EnumType, Field, PrimitiveType, Range, Schema, Type};
use syn::{
    Data, DataStruct, DeriveInput, Expr, ExprLit, Field as SynField, GenericArgument, Lit, Meta,
    PathArguments, Type as SynType, TypePath, punctuated::Punctuated, token::Comma,
};

pub fn parse_toon_schema(input: &DeriveInput) -> syn::Result<Schema> {
    let struct_name = input.ident.to_string();

    // Parse struct-level attributes
    let mut name = None;
    let mut description = None;
    for attr in &input.attrs {
        if attr.path().is_ident("toon_schema") {
            let metas = attr.parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)?;
            for meta in metas {
                if let Meta::NameValue(nv) = meta {
                    if nv.path.is_ident("name")
                        && let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                    {
                        name = Some(s.value());
                    } else if nv.path.is_ident("description")
                        && let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                    {
                        description = Some(s.value());
                    }
                }
            }
        }
    }

    // Use custom name if provided, otherwise use struct name
    let name = name.unwrap_or(struct_name);

    // Parse fields
    let fields = match &input.data {
        Data::Struct(DataStruct { fields, .. }) => fields
            .iter()
            .map(parse_field)
            .collect::<syn::Result<Vec<_>>>()?
            .into_iter()
            .flatten() // Filter out skipped fields
            .collect(),
        _ => {
            return Err(syn::Error::new(
                input.ident.span(),
                "ToonSchema can only be derived for structs",
            ));
        }
    };

    Ok(Schema {
        name,
        description,
        fields,
    })
}

fn parse_field(field: &SynField) -> syn::Result<Option<Field>> {
    let mut name = field.ident.as_ref().unwrap().to_string();
    let mut description = None;
    let mut example = None;
    let mut skipped = false;
    let mut is_option = false;
    let mut is_vec = false;
    let mut min: Option<u32> = None;
    let mut max: Option<u32> = None;
    let mut default_value: Option<String> = None;
    let field_span = field.ident.as_ref().unwrap().span();

    // Parse attributes
    for attr in &field.attrs {
        // Handle serde attributes
        if attr.path().is_ident("serde") {
            let metas = attr.parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)?;
            for meta in metas {
                match meta {
                    Meta::NameValue(nv) if nv.path.is_ident("rename") => {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                        {
                            name = s.value();
                        }
                    }
                    Meta::Path(path) if path.is_ident("skip") => {
                        skipped = true;
                    }
                    Meta::Path(path) if path.is_ident("default") => {
                        // #[serde(default)] - use default value of the type
                        default_value = Some(String::from("default"));
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("default") => {
                        // #[serde(default = "path")] - store the path for runtime resolution
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                        {
                            default_value = Some(format!("{}", s.value()));
                        }
                    }
                    _ => {}
                }
            }
        }

        // Handle toon_schema attributes
        if attr.path().is_ident("toon_schema") {
            let metas = attr.parse_args_with(Punctuated::<Meta, Comma>::parse_terminated)?;
            for meta in metas {
                match meta {
                    Meta::NameValue(nv) if nv.path.is_ident("description") => {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                        {
                            description = Some(s.value());
                        }
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("example") => {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                        {
                            // Validate that the example is valid JSON at compile time
                            let example_str = s.value();
                            match serde_json::from_str::<serde_json::Value>(&example_str) {
                                Ok(_) => {
                                    example = Some(example_str);
                                }
                                Err(e) => {
                                    return Err(syn::Error::new(
                                        s.span(),
                                        format!(
                                            "Invalid JSON in example: {}. \
                                            Examples must be valid JSON matching the field type. \
                                            For strings use: \"value\", \
                                            for numbers: 42, \
                                            for booleans: true/false, \
                                            for arrays: [1, 2, 3]",
                                            e
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("min") => {
                        // Parse min = N
                        if let Some(n) = parse_u32_expr(&nv.value) {
                            min = Some(n);
                        }
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("max") => {
                        // Parse max = N
                        if let Some(n) = parse_u32_expr(&nv.value) {
                            max = Some(n);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // If skipped, return None (filter out this field)
    if skipped {
        return Ok(None);
    }

    // Check for reserved word "tool"
    if name == "tool" {
        return Err(syn::Error::new(
            field_span,
            "field name 'tool' is reserved\n\
             hint: 'tool' is a reserved keyword in the Toon format",
        ));
    }

    // Analyze type
    let ty = analyze_type(&field.ty, &mut is_option, &mut is_vec, field_span)?;

    // Validate: custom ranges only allowed on Vec types
    if (min.is_some() || max.is_some()) && !is_vec {
        return Err(syn::Error::new(
            field_span,
            format!(
                "custom ranges (min/max) are only valid for Vec<T> fields\n\
                 help: field '{}' has type {:?}\n\
                 hint: remove min/max attributes or change type to Vec<T>",
                field.ident.as_ref().unwrap(),
                quote::quote!(#field.ty)
            ),
        ));
    }

    // Determine range based on type and custom range attributes
    let range = if is_option {
        Range::ZeroToOne
    } else if is_vec {
        match (min, max) {
            (Some(m), None) => Range::AtLeast(m),
            (Some(m), Some(n)) => Range::Bounded(m, n),
            (None, Some(n)) => Range::Bounded(0, n),
            (None, None) => Range::ZeroOrMore,
        }
    } else {
        Range::Exactly(1)
    };

    // Validate that example is provided (compile-time check)
    let field_ident = field.ident.as_ref().unwrap();
    let example = example.ok_or_else(|| {
        syn::Error::new(
            field_span,
            format!(
                "Field '{}' is missing required #[toon_schema(example = \"...\")] attribute\n\
                 help: every field must have an example for documentation\n\
                 example: #[toon_schema(example = \"{}\")]",
                field_ident,
                get_example_for_type(&ty)
            ),
        )
    })?;

    Ok(Some(Field {
        name,
        ty,
        description,
        example,
        range,
        skipped: false, // We filtered these out above
        default_value,
    }))
}

fn get_example_for_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => match p {
            PrimitiveType::String => "\"example_value\"".to_string(),
            PrimitiveType::Integer => "42".to_string(),
            PrimitiveType::Float => "3.14".to_string(),
            PrimitiveType::Boolean => "true".to_string(),
        },
        Type::Array(_) => "[1, 2, 3]".to_string(),
        Type::Enum(enum_ty) => {
            if let Some(first) = enum_ty.variants.first() {
                format!("\"{}\"", first)
            } else {
                "\"\"".to_string()
            }
        }
    }
}

fn parse_u32_expr(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => i.base10_parse::<u32>().ok(),
        _ => None,
    }
}

fn analyze_type(ty: &SynType, is_option: &mut bool, is_vec: &mut bool, span: proc_macro2::Span) -> syn::Result<Type> {
    match ty {
        SynType::Path(TypePath { path, .. }) => {
            let segment = path.segments.last().unwrap();
            let ident = segment.ident.to_string();

            match ident.as_str() {
                "String" => Ok(Type::Primitive(PrimitiveType::String)),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Ok(Type::Primitive(PrimitiveType::Integer)),
                "f32" | "f64" => Ok(Type::Primitive(PrimitiveType::Float)),
                "bool" => Ok(Type::Primitive(PrimitiveType::Boolean)),
                "Option" => {
                    *is_option = true;
                    // Extract inner type
                    if let PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(GenericArgument::Type(inner_ty)) = args.args.first() {
                            analyze_type(inner_ty, is_option, is_vec, span)
                        } else {
                            Err(syn::Error::new(
                                span,
                                "Option must have a type parameter\n\
                                 help: use Option<T> where T is a supported type",
                            ))
                        }
                    } else {
                        Err(syn::Error::new(
                            span,
                            "Option must have a type parameter\n\
                             help: use Option<T> where T is a supported type",
                        ))
                    }
                }
                "Vec" => {
                    *is_vec = true;
                    // Extract inner type
                    if let PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner = analyze_type(inner_ty, is_option, is_vec, span)?;
                            Ok(Type::Array(Box::new(inner)))
                        } else {
                            Err(syn::Error::new(
                                span,
                                "Vec must have a type parameter\n\
                                 help: use Vec<T> where T is a supported type",
                            ))
                        }
                    } else {
                        Err(syn::Error::new(
                            span,
                            "Vec must have a type parameter\n\
                             help: use Vec<T> where T is a supported type",
                        ))
                    }
                }
                _ => {
                    // Unknown type - error with helpful message
                    let supported_types = "String, i8-i128, u8-u128, f32, f64, bool, Vec<T>, Option<T>, or enum types";
                    Err(syn::Error::new(
                        span,
                        format!(
                            "unsupported type '{}'\n\
                             help: supported types are: {}\n\
                             note: custom types, nested structs, and maps are not supported",
                            ident, supported_types
                        ),
                    ))
                }
            }
        }
        _ => Err(syn::Error::new(
            span,
            "unsupported type\n\
             help: supported types are: String, i8-i128, u8-u128, f32, f64, bool, Vec<T>, Option<T>, or enum types",
        )),
    }
}
