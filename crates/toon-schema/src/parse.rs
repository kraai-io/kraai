use crate::ir::{Field, PrimitiveType, Range, Schema, Type};
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
                        // #[serde(default = "path")] - use custom default
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
                            example = Some(s.value());
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
            field.ident.as_ref().unwrap().span(),
            "field name 'tool' is reserved",
        ));
    }

    // Analyze type
    let ty = analyze_type(&field.ty, &mut is_option, &mut is_vec)?;

    // Validate: custom ranges only allowed on Vec types
    if (min.is_some() || max.is_some()) && !is_vec {
        return Err(syn::Error::new(
            field.ident.as_ref().unwrap().span(),
            "custom ranges can only be applied to Vec<T> fields",
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
            field_ident.span(),
            format!(
                "Field '{}' must have a #[toon_schema(example = \"...\")] attribute",
                field_ident
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

fn parse_u32_expr(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => i.base10_parse::<u32>().ok(),
        _ => None,
    }
}

fn analyze_type(ty: &SynType, is_option: &mut bool, is_vec: &mut bool) -> syn::Result<Type> {
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
                            analyze_type(inner_ty, is_option, is_vec)
                        } else {
                            Err(syn::Error::new_spanned(
                                ty,
                                "Option must have a type parameter",
                            ))
                        }
                    } else {
                        Err(syn::Error::new_spanned(
                            ty,
                            "Option must have a type parameter",
                        ))
                    }
                }
                "Vec" => {
                    *is_vec = true;
                    // Extract inner type
                    if let PathArguments::AngleBracketed(args) = &segment.arguments {
                        if let Some(GenericArgument::Type(inner_ty)) = args.args.first() {
                            let inner = analyze_type(inner_ty, is_option, is_vec)?;
                            Ok(Type::Array(Box::new(inner)))
                        } else {
                            Err(syn::Error::new_spanned(
                                ty,
                                "Vec must have a type parameter",
                            ))
                        }
                    } else {
                        Err(syn::Error::new_spanned(
                            ty,
                            "Vec must have a type parameter",
                        ))
                    }
                }
                _ => {
                    // For now, treat unknown types as strings
                    // This could be extended to support custom types
                    Ok(Type::Primitive(PrimitiveType::String))
                }
            }
        }
        _ => Err(syn::Error::new_spanned(ty, "Unsupported type")),
    }
}
