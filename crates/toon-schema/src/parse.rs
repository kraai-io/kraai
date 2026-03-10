//! Parser module for the toon-schema derive macro.
//!
//! This module handles parsing Rust struct definitions and their attributes
//! into an intermediate representation (IR) used for schema generation.
//!
//! # Supported Attributes
//!
//! ## Struct-level (`#[toon_schema(...)]`)
//! - `name = "..."` - Custom tool name
//! - `description = "..."` - Tool description
//! - `example = "..."` - **Required, repeatable.** Full JSON object example
//!
//! ## Field-level (`#[toon_schema(...)]`)
//! - `description = "..."` - Field description
//! - `min = N` - Minimum count for Vec fields
//! - `max = N` - Maximum count for Vec fields
//!
//! ## Serde Integration
//! The parser also respects serde attributes:
//! - `#[serde(rename = "...")]` - Rename field in output
//! - `#[serde(skip)]` - Skip field in schema
//! - `#[serde(default)]` - Use default value
//! - `#[serde(default = "path")]` - Use custom default function

use crate::ir::{Field, PrimitiveType, Range, Schema, Type};
use serde_json::{Map, Value};
use syn::{
    Data, DataStruct, DeriveInput, Expr, ExprLit, Field as SynField, Fields, GenericArgument, Lit,
    Meta, PathArguments, Type as SynType, TypePath, punctuated::Punctuated, spanned::Spanned,
    token::Comma,
};

pub fn parse_toon_schema(input: &DeriveInput) -> syn::Result<Schema> {
    let struct_name = input.ident.to_string();

    let mut name = None;
    let mut description = None;
    let mut examples = Vec::new();
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
                    } else if nv.path.is_ident("example")
                        && let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                    {
                        let example_str = s.value();
                        match serde_json::from_str::<Value>(&example_str) {
                            Ok(_) => examples.push(example_str),
                            Err(error) => {
                                return Err(syn::Error::new(
                                    s.span(),
                                    format!(
                                        "Invalid JSON in example: {}. Struct-level examples must be valid JSON objects.",
                                        error
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    let name = name.unwrap_or(struct_name);

    let fields: Vec<Field> = match &input.data {
        Data::Struct(DataStruct { fields, .. }) => {
            let named_fields = match fields {
                Fields::Named(fields) => &fields.named,
                Fields::Unnamed(_) => {
                    return Err(syn::Error::new(
                        input.ident.span(),
                        "ToonSchema can only be derived for structs with named fields\n\
                         help: change the struct to use named fields like `struct Tool { field: String }`",
                    ));
                }
                Fields::Unit => {
                    return Err(syn::Error::new(
                        input.ident.span(),
                        "ToonSchema cannot be derived for unit structs\n\
                         help: add at least one named field or remove the derive",
                    ));
                }
            };

            named_fields
                .iter()
                .map(parse_field)
                .collect::<syn::Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect()
        }
        _ => {
            return Err(syn::Error::new(
                input.ident.span(),
                "ToonSchema can only be derived for structs",
            ));
        }
    };

    if fields.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "ToonSchema cannot be derived for empty structs\n\
             help: add at least one field or remove the derive",
        ));
    }

    if examples.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            format!(
                "Struct '{}' is missing required #[toon_schema(example = \"...\")] attribute\n\
                 help: add one or more full JSON object examples at the struct level\n\
                 example: #[toon_schema(example = r#\"{}\"#)]",
                input.ident,
                get_struct_example_for_fields(&fields)
            ),
        ));
    }

    let schema = Schema {
        name,
        description,
        fields,
        examples,
    };
    validate_schema_examples(&schema, input.ident.span())?;

    Ok(schema)
}

fn parse_field(field: &SynField) -> syn::Result<Option<Field>> {
    let field_ident = field.ident.as_ref().ok_or_else(|| {
        syn::Error::new(
            field.ty.span(),
            "ToonSchema can only be derived for structs with named fields",
        )
    })?;
    let mut name = field_ident.to_string();
    let mut description = None;
    let mut skipped = false;
    let mut is_option = false;
    let mut is_vec = false;
    let mut min: Option<u32> = None;
    let mut max: Option<u32> = None;
    let mut default_value: Option<String> = None;
    let field_span = field_ident.span();

    for attr in &field.attrs {
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
                        default_value = Some(String::from("default"));
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("default") => {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) = &nv.value
                        {
                            default_value = Some(s.value().to_string());
                        }
                    }
                    _ => {}
                }
            }
        }

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
                        return Err(syn::Error::new(
                            nv.path.span(),
                            "field-level #[toon_schema(example = ...)] is no longer supported; move examples to the struct as full JSON objects",
                        ));
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("min") => {
                        if let Some(n) = parse_u32_expr(&nv.value) {
                            min = Some(n);
                        }
                    }
                    Meta::NameValue(nv) if nv.path.is_ident("max") => {
                        if let Some(n) = parse_u32_expr(&nv.value) {
                            max = Some(n);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if skipped {
        return Ok(None);
    }

    if name == "tool" {
        return Err(syn::Error::new(
            field_span,
            "field name 'tool' is reserved\n\
             hint: 'tool' is a reserved keyword in the Toon format",
        ));
    }

    let ty = analyze_type(&field.ty, &mut is_option, &mut is_vec, field_span)?;

    if (min.is_some() || max.is_some()) && !is_vec {
        return Err(syn::Error::new(
            field_span,
            format!(
                "custom ranges (min/max) are only valid for Vec<T> fields\n\
                 help: field '{}' has type {:?}\n\
                 hint: remove min/max attributes or change type to Vec<T>",
                field_ident,
                quote::quote!(#field.ty)
            ),
        ));
    }

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

    Ok(Some(Field {
        name,
        ty,
        description,
        range,
        skipped: false,
        default_value,
        optional: is_option,
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
    }
}

fn get_struct_example_for_fields(fields: &[Field]) -> String {
    let mut parts = Vec::new();
    for field in fields {
        if field.optional || field.default_value.is_some() {
            continue;
        }
        parts.push(format!("\"{}\":{}", field.name, get_example_for_type(&field.ty)));
    }
    format!("{{{}}}", parts.join(","))
}

fn parse_u32_expr(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(i), ..
        }) => i.base10_parse::<u32>().ok(),
        _ => None,
    }
}

fn analyze_type(
    ty: &SynType,
    is_option: &mut bool,
    is_vec: &mut bool,
    span: proc_macro2::Span,
) -> syn::Result<Type> {
    match ty {
        SynType::Path(TypePath { path, .. }) => {
            let segment = path.segments.last().ok_or_else(|| {
                syn::Error::new(
                    span,
                    "unsupported path type\n\
                     help: use a concrete supported type like String, bool, Option<T>, or Vec<T>",
                )
            })?;
            let ident = segment.ident.to_string();

            match ident.as_str() {
                "String" => Ok(Type::Primitive(PrimitiveType::String)),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => Ok(Type::Primitive(PrimitiveType::Integer)),
                "f32" | "f64" => Ok(Type::Primitive(PrimitiveType::Float)),
                "bool" => Ok(Type::Primitive(PrimitiveType::Boolean)),
                "Option" => {
                    *is_option = true;
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
                    let supported_types =
                        "String, i8-i128, u8-u128, f32, f64, bool, Vec<T>, or Option<T>";
                    Err(syn::Error::new(
                        span,
                        format!(
                            "unsupported type '{}'\n\
                             help: supported types are: {}",
                            ident, supported_types
                        ),
                    ))
                }
            }
        }
        _ => Err(syn::Error::new(
            span,
            "unsupported type\n\
             help: supported types are: String, i8-i128, u8-u128, f32, f64, bool, Vec<T>, or Option<T>",
        )),
    }
}

fn validate_schema_examples(schema: &Schema, span: proc_macro2::Span) -> syn::Result<()> {
    for (index, example) in schema.examples.iter().enumerate() {
        let value: Value = serde_json::from_str(example).map_err(|error| {
            syn::Error::new(
                span,
                format!("example {} is invalid JSON: {}", index + 1, error),
            )
        })?;

        let obj = value.as_object().ok_or_else(|| {
            syn::Error::new(
                span,
                format!(
                    "example {} must be a JSON object, got {}",
                    index + 1,
                    describe_json_value(&value)
                ),
            )
        })?;

        validate_example_object(schema, index + 1, obj, span)?;
    }

    Ok(())
}

fn validate_example_object(
    schema: &Schema,
    example_number: usize,
    obj: &Map<String, Value>,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    for key in obj.keys() {
        if schema.fields.iter().any(|field| field.name == *key) {
            continue;
        }

        return Err(syn::Error::new(
            span,
            format!(
                "example {} contains unknown field '{}' ; expected one of: {}",
                example_number,
                key,
                schema
                    .fields
                    .iter()
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }

    for field in &schema.fields {
        match obj.get(&field.name) {
            Some(value) => validate_field_value(field, value, example_number, span)?,
            None if field.optional || field.default_value.is_some() => {}
            None => {
                return Err(syn::Error::new(
                    span,
                    format!(
                        "example {} is missing required field '{}' (expected {})",
                        example_number,
                        field.name,
                        describe_type(&field.ty)
                    ),
                ));
            }
        }
    }

    Ok(())
}

fn validate_field_value(
    field: &Field,
    value: &Value,
    example_number: usize,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    if value.is_null() {
        if field.optional {
            return Ok(());
        }

        return Err(syn::Error::new(
            span,
            format!(
                "example {} field '{}' does not allow null; expected {}",
                example_number,
                field.name,
                describe_type(&field.ty)
            ),
        ));
    }

    validate_value_against_type(&field.ty, value, example_number, &field.name, span)?;

    if let (Type::Array(_), Value::Array(arr)) = (&field.ty, value) {
        validate_array_range(field, arr.len(), example_number, span)?;
    }

    Ok(())
}

fn validate_value_against_type(
    ty: &Type,
    value: &Value,
    example_number: usize,
    field_name: &str,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    match (ty, value) {
        (Type::Primitive(PrimitiveType::String), Value::String(_)) => Ok(()),
        (Type::Primitive(PrimitiveType::Integer), Value::Number(number))
            if number.is_i64() || number.is_u64() =>
        {
            Ok(())
        }
        (Type::Primitive(PrimitiveType::Float), Value::Number(_)) => Ok(()),
        (Type::Primitive(PrimitiveType::Boolean), Value::Bool(_)) => Ok(()),
        (Type::Array(inner), Value::Array(values)) => {
            for item in values {
                validate_value_against_type(inner, item, example_number, field_name, span)?;
            }
            Ok(())
        }
        _ => Err(syn::Error::new(
            span,
            format!(
                "example {} field '{}' has wrong type; expected {}, got {}",
                example_number,
                field_name,
                describe_type(ty),
                describe_json_value(value)
            ),
        )),
    }
}

fn validate_array_range(
    field: &Field,
    len: usize,
    example_number: usize,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    let len = len as u32;
    let valid = match field.range {
        Range::Exactly(expected) => len == expected,
        Range::ZeroToOne => len <= 1,
        Range::ZeroOrMore => true,
        Range::AtLeast(min) => len >= min,
        Range::Bounded(min, max) => len >= min && len <= max,
    };

    if valid {
        Ok(())
    } else {
        Err(syn::Error::new(
            span,
            format!(
                "example {} field '{}' has {} items, which violates range {}",
                example_number,
                field.name,
                len,
                field.range.format()
            ),
        ))
    }
}

fn describe_type(ty: &Type) -> String {
    match ty {
        Type::Primitive(PrimitiveType::String) => "string".to_string(),
        Type::Primitive(PrimitiveType::Integer) => "integer".to_string(),
        Type::Primitive(PrimitiveType::Float) => "float".to_string(),
        Type::Primitive(PrimitiveType::Boolean) => "boolean".to_string(),
        Type::Array(inner) => format!("array<{}>", describe_type(inner)),
    }
}

fn describe_json_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                "integer".to_string()
            } else {
                "number".to_string()
            }
        }
        Value::String(_) => "string".to_string(),
        Value::Array(_) => "array".to_string(),
        Value::Object(_) => "object".to_string(),
    }
}
