use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Number, Value};
use syn::parse::{Parse, ParseStream};
use syn::{
    Attribute, Expr, ExprLit, Field, Fields, GenericArgument, Ident, Item, ItemStruct, Lit,
    LitBool, LitFloat, LitInt, LitStr, Meta, PathArguments, Result, Token, Type, TypeArray,
    TypePath, braced, bracketed, punctuated::Punctuated, spanned::Spanned, token,
};

use crate::ir::{
    ExampleObject, FieldDef, FieldType, ObjectDef, PrimitiveType, Range, ToolSchema, TypeItem,
};

pub fn parse_tool_schema(input: ParseStream<'_>) -> Result<ToolSchema> {
    let parsed = ToolMacroInput::parse(input)?;
    build_tool_schema(parsed)
}

struct ToolMacroInput {
    name: String,
    description: Option<String>,
    type_items: Vec<ItemStruct>,
    root: Ident,
    examples: Vec<ExampleObject>,
}

impl Parse for ToolMacroInput {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut name = None;
        let mut description = None;
        let mut type_items = None;
        let mut root = None;
        let mut examples = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;

            match key.to_string().as_str() {
                "name" => {
                    let value: LitStr = input.parse()?;
                    name = Some(value.value());
                }
                "description" => {
                    let value: LitStr = input.parse()?;
                    description = Some(value.value());
                }
                "types" => {
                    let content;
                    braced!(content in input);
                    let mut items = Vec::new();
                    while !content.is_empty() {
                        match content.parse::<Item>()? {
                            Item::Struct(item) => items.push(item),
                            Item::Enum(item) => {
                                return Err(syn::Error::new(
                                    item.span(),
                                    "enums are not supported in `toon_tool!`",
                                ));
                            }
                            other => {
                                return Err(syn::Error::new(
                                    other.span(),
                                    "only named structs are supported in `types:`",
                                ));
                            }
                        }
                    }
                    type_items = Some(items);
                }
                "root" => {
                    root = Some(input.parse()?);
                }
                "examples" => {
                    let content;
                    bracketed!(content in input);
                    let parsed = Punctuated::<ExampleExpr, Token![,]>::parse_terminated(&content)?;
                    examples = Some(
                        parsed
                            .into_iter()
                            .map(|example| ExampleObject {
                                value: Value::Object(example.value),
                            })
                            .collect(),
                    );
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown `toon_tool!` key `{other}`"),
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            name: name.ok_or_else(|| syn::Error::new(input.span(), "missing `name`"))?,
            description,
            type_items: type_items.ok_or_else(|| syn::Error::new(input.span(), "missing `types`"))?,
            root: root.ok_or_else(|| syn::Error::new(input.span(), "missing `root`"))?,
            examples: examples.ok_or_else(|| syn::Error::new(input.span(), "missing `examples`"))?,
        })
    }
}

fn build_tool_schema(parsed: ToolMacroInput) -> Result<ToolSchema> {
    let mut seen_names = BTreeSet::new();
    let mut defs = Vec::with_capacity(parsed.type_items.len());

    for mut item in parsed.type_items {
        let def = parse_struct_def(&item, &seen_names)?;
        if !seen_names.insert(def.name.clone()) {
            return Err(syn::Error::new(
                item.ident.span(),
                format!("duplicate type `{}` in `types:`", def.name),
            ));
        }
        strip_toon_schema_attrs(&mut item);
        defs.push(TypeItem { item, def });
    }

    let root = parsed.root.to_string();
    if !defs.iter().any(|item| item.def.name == root) {
        return Err(syn::Error::new(
            parsed.root.span(),
            format!("root type `{root}` must be declared in `types:`"),
        ));
    }

    let schema = ToolSchema {
        name: parsed.name,
        description: parsed.description,
        root,
        types: defs,
        examples: parsed.examples,
    };

    validate_examples(&schema)?;
    Ok(schema)
}

fn strip_toon_schema_attrs(item: &mut ItemStruct) {
    item.attrs.retain(|attr| !attr.path().is_ident("toon_schema"));
    if let Fields::Named(fields) = &mut item.fields {
        for field in &mut fields.named {
            field.attrs.retain(|attr| !attr.path().is_ident("toon_schema"));
        }
    }
}

fn parse_struct_def(item: &ItemStruct, declared: &BTreeSet<String>) -> Result<ObjectDef> {
    let fields = match &item.fields {
        Fields::Named(fields) => &fields.named,
        Fields::Unnamed(_) => {
            return Err(syn::Error::new(
                item.span(),
                "tuple structs are not supported in `toon_tool!`",
            ));
        }
        Fields::Unit => {
            return Err(syn::Error::new(
                item.span(),
                "unit structs are not supported in `toon_tool!`",
            ));
        }
    };

    if fields.is_empty() {
        return Err(syn::Error::new(
            item.span(),
            "empty structs are not supported in `toon_tool!`",
        ));
    }

    let parsed_fields = fields
        .iter()
        .map(|field| parse_field_def(field, declared))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(ObjectDef {
        name: item.ident.to_string(),
        fields: parsed_fields,
    })
}

fn parse_field_def(field: &Field, declared: &BTreeSet<String>) -> Result<Option<FieldDef>> {
    let ident = field.ident.as_ref().ok_or_else(|| {
        syn::Error::new(
            field.span(),
            "only named fields are supported in `toon_tool!`",
        )
    })?;

    let mut visible_name = ident.to_string();
    let mut description = None;
    let mut min = None;
    let mut max = None;
    let mut skipped = false;
    let mut default_value = None;

    for attr in &field.attrs {
        if attr.path().is_ident("serde") {
            parse_serde_attr(
                attr,
                &mut visible_name,
                &mut skipped,
                &mut default_value,
            )?;
        } else if attr.path().is_ident("toon_schema") {
            parse_toon_field_attr(attr, &mut description, &mut min, &mut max)?;
        }
    }

    if skipped {
        return Ok(None);
    }

    if visible_name == "tool" {
        return Err(syn::Error::new(
            ident.span(),
            "field name `tool` is reserved in TOON tool schemas",
        ));
    }

    let (ty, range) = parse_type_with_range(&field.ty, min, max, declared)?;

    Ok(Some(FieldDef {
        visible_name,
        description,
        ty,
        range,
        default_value,
        skipped: false,
    }))
}

fn parse_serde_attr(
    attr: &Attribute,
    visible_name: &mut String,
    skipped: &mut bool,
    default_value: &mut Option<String>,
) -> Result<()> {
    let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    for meta in metas {
        match meta {
            Meta::NameValue(nv) if nv.path.is_ident("rename") => {
                if let Expr::Lit(ExprLit {
                    lit: Lit::Str(value),
                    ..
                }) = nv.value
                {
                    *visible_name = value.value();
                }
            }
            Meta::Path(path) if path.is_ident("skip") => {
                *skipped = true;
            }
            Meta::Path(path) if path.is_ident("default") => {
                *default_value = Some(String::from("default"));
            }
            Meta::NameValue(nv) if nv.path.is_ident("default") => {
                if let Expr::Lit(ExprLit {
                    lit: Lit::Str(value),
                    ..
                }) = nv.value
                {
                    *default_value = Some(value.value());
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_toon_field_attr(
    attr: &Attribute,
    description: &mut Option<String>,
    min: &mut Option<u32>,
    max: &mut Option<u32>,
) -> Result<()> {
    let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
    for meta in metas {
        match meta {
            Meta::NameValue(nv) if nv.path.is_ident("description") => {
                if let Expr::Lit(ExprLit {
                    lit: Lit::Str(value),
                    ..
                }) = nv.value
                {
                    *description = Some(value.value());
                }
            }
            Meta::NameValue(nv) if nv.path.is_ident("min") => {
                *min = parse_u32_expr(&nv.value);
            }
            Meta::NameValue(nv) if nv.path.is_ident("max") => {
                *max = parse_u32_expr(&nv.value);
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_u32_expr(expr: &Expr) -> Option<u32> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse().ok(),
        _ => None,
    }
}

fn parse_type_with_range(
    ty: &Type,
    min: Option<u32>,
    max: Option<u32>,
    declared: &BTreeSet<String>,
) -> Result<(FieldType, Range)> {
    if let Some(inner) = option_inner(ty) {
        if min.is_some() || max.is_some() {
            return Err(syn::Error::new(
                ty.span(),
                "custom ranges (min/max) are not supported on Option<T> fields",
            ));
        }
        let inner_ty = parse_value_type(inner, declared)?;
        return Ok((inner_ty, Range::ZeroToOne));
    }

    if let Some(inner) = vec_inner(ty) {
        let inner_ty = parse_value_type(inner, declared)?;
        return Ok((
            FieldType::Array(Box::new(inner_ty)),
            match (min, max) {
                (Some(lower), Some(upper)) => Range::Bounded(lower, upper),
                (Some(lower), None) => Range::AtLeast(lower),
                (None, Some(upper)) => Range::Bounded(0, upper),
                (None, None) => Range::ZeroOrMore,
            },
        ));
    }

    if let Some((inner, len)) = fixed_array_inner(ty)? {
        if min.is_some() || max.is_some() {
            return Err(syn::Error::new(
                ty.span(),
                "custom ranges (min/max) are not supported on fixed-size arrays",
            ));
        }
        let inner_ty = parse_value_type(inner, declared)?;
        return Ok((FieldType::Array(Box::new(inner_ty)), Range::Exactly(len)));
    }

    let base_ty = parse_value_type(ty, declared)?;
    if min.is_some() || max.is_some() {
        return Err(syn::Error::new(
            ty.span(),
            "custom ranges (min/max) are only valid for Vec<T> fields",
        ));
    }
    Ok((base_ty, Range::Exactly(1)))
}

fn parse_value_type(ty: &Type, declared: &BTreeSet<String>) -> Result<FieldType> {
    if let Some(inner) = option_inner(ty) {
        return parse_value_type(inner, declared);
    }

    if let Some(inner) = vec_inner(ty) {
        return Ok(FieldType::Array(Box::new(parse_value_type(inner, declared)?)));
    }

    if let Some((inner, _)) = fixed_array_inner(ty)? {
        return Ok(FieldType::Array(Box::new(parse_value_type(inner, declared)?)));
    }

    if let Some(value) = map_value_type(ty)? {
        return Ok(FieldType::Map(Box::new(parse_value_type(value, declared)?)));
    }

    match ty {
        Type::Path(TypePath { path, .. }) => {
            let segment = path.segments.last().ok_or_else(|| {
                syn::Error::new(ty.span(), "unsupported type")
            })?;

            let ident = segment.ident.to_string();
            match ident.as_str() {
                "String" => Ok(FieldType::Primitive(PrimitiveType::String)),
                "bool" => Ok(FieldType::Primitive(PrimitiveType::Boolean)),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32"
                | "u64" | "u128" | "usize" => Ok(FieldType::Primitive(PrimitiveType::Integer)),
                "f32" | "f64" => Ok(FieldType::Primitive(PrimitiveType::Float)),
                other if declared.contains(other) => Ok(FieldType::Object(other.to_string())),
                other => Err(syn::Error::new(
                    ty.span(),
                    format!(
                        "unsupported external type `{other}`; declare nested types inside `types:`"
                    ),
                )),
            }
        }
        _ => Err(syn::Error::new(
            ty.span(),
            "unsupported type syntax in `toon_tool!`",
        )),
    }
}

fn option_inner(ty: &Type) -> Option<&Type> {
    path_generic_inner(ty, "Option")
}

fn vec_inner(ty: &Type) -> Option<&Type> {
    path_generic_inner(ty, "Vec")
}

fn path_generic_inner<'a>(ty: &'a Type, expected: &str) -> Option<&'a Type> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return None;
    };

    let segment = path.segments.last()?;
    if segment.ident != expected {
        return None;
    }

    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };

    match args.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

fn fixed_array_inner(ty: &Type) -> Result<Option<(&Type, u32)>> {
    let Type::Array(TypeArray { elem, len, .. }) = ty else {
        return Ok(None);
    };

    let Expr::Lit(ExprLit {
        lit: Lit::Int(len_lit),
        ..
    }) = len
    else {
        return Err(syn::Error::new(
            len.span(),
            "fixed-size array lengths must be integer literals",
        ));
    };

    Ok(Some((&**elem, len_lit.base10_parse()?)))
}

fn map_value_type(ty: &Type) -> Result<Option<&Type>> {
    let Type::Path(TypePath { path, .. }) = ty else {
        return Ok(None);
    };

    let segment = match path.segments.last() {
        Some(segment) if segment.ident == "HashMap" || segment.ident == "BTreeMap" => segment,
        _ => return Ok(None),
    };

    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new(
            ty.span(),
            "map types must declare key and value types",
        ));
    };

    let mut iter = args.args.iter();
    let Some(GenericArgument::Type(key_ty)) = iter.next() else {
        return Err(syn::Error::new(ty.span(), "map key type is missing"));
    };
    let Some(GenericArgument::Type(value_ty)) = iter.next() else {
        return Err(syn::Error::new(ty.span(), "map value type is missing"));
    };

    match key_ty {
        Type::Path(TypePath { path, .. }) if path.is_ident("String") => Ok(Some(value_ty)),
        _ => Err(syn::Error::new(
            key_ty.span(),
            "only `HashMap<String, T>` and `BTreeMap<String, T>` are supported",
        )),
    }
}

struct ExampleExpr {
    value: Map<String, Value>,
}

impl Parse for ExampleExpr {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let object = ExampleObjectExpr::parse(input)?;
        Ok(Self { value: object.value })
    }
}

struct ExampleObjectExpr {
    value: Map<String, Value>,
}

impl Parse for ExampleObjectExpr {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let content;
        braced!(content in input);
        let mut map = Map::new();
        let mut seen = BTreeSet::new();

        while !content.is_empty() {
            let key = parse_example_key(&content)?;
            if !seen.insert(key.clone()) {
                return Err(syn::Error::new(
                    content.span(),
                    format!("duplicate example key `{key}`"),
                ));
            }
            content.parse::<Token![:]>()?;
            let value = parse_example_value(&content)?;
            map.insert(key, value);

            if content.peek(Token![,]) {
                content.parse::<Token![,]>()?;
            }
        }

        Ok(Self { value: map })
    }
}

fn parse_example_key(input: ParseStream<'_>) -> Result<String> {
    if input.peek(LitStr) {
        return Ok(input.parse::<LitStr>()?.value());
    }
    Ok(input.parse::<Ident>()?.to_string())
}

fn parse_example_value(input: ParseStream<'_>) -> Result<Value> {
    if input.peek(LitStr) {
        return Ok(Value::String(input.parse::<LitStr>()?.value()));
    }
    if input.peek(LitBool) {
        return Ok(Value::Bool(input.parse::<LitBool>()?.value));
    }
    if input.peek(LitInt) {
        let lit = input.parse::<LitInt>()?;
        let value = lit.base10_parse::<i64>()?;
        return Ok(Value::Number(Number::from(value)));
    }
    if input.peek(LitFloat) {
        let lit = input.parse::<LitFloat>()?;
        let value = lit.base10_parse::<f64>()?;
        let number = Number::from_f64(value).ok_or_else(|| {
            syn::Error::new(lit.span(), "floating-point example must be finite")
        })?;
        return Ok(Value::Number(number));
    }
    if input.peek(token::Bracket) {
        let content;
        bracketed!(content in input);
        let mut items = Vec::new();
        while !content.is_empty() {
            items.push(parse_example_value(&content)?);
            if content.peek(Token![,]) {
                content.parse::<Token![,]>()?;
            }
        }
        return Ok(Value::Array(items));
    }
    if input.peek(token::Brace) {
        let object = ExampleObjectExpr::parse(input)?;
        return Ok(Value::Object(object.value));
    }

    let ident: Ident = input.parse()?;
    match ident.to_string().as_str() {
        "null" => Ok(Value::Null),
        other => Err(syn::Error::new(
            ident.span(),
            format!("unsupported example value `{other}`"),
        )),
    }
}

fn validate_examples(schema: &ToolSchema) -> Result<()> {
    let defs = schema
        .types
        .iter()
        .map(|item| (item.def.name.clone(), item.def.clone()))
        .collect::<BTreeMap<_, _>>();

    let root = defs.get(&schema.root).ok_or_else(|| {
        syn::Error::new(proc_macro2::Span::call_site(), "missing root definition")
    })?;

    for example in &schema.examples {
        let Value::Object(object) = &example.value else {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "tool examples must be objects",
            ));
        };
        validate_object(root, object, &defs)?;
    }

    Ok(())
}

fn validate_object(
    def: &ObjectDef,
    object: &Map<String, Value>,
    defs: &BTreeMap<String, ObjectDef>,
) -> Result<()> {
    for key in object.keys() {
        if def.fields.iter().any(|field| !field.skipped && field.visible_name == *key) {
            continue;
        }

        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "example contains unknown field `{key}`; expected one of: {}",
                def.fields
                    .iter()
                    .filter(|field| !field.skipped)
                    .map(|field| field.visible_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }

    for field in def.fields.iter().filter(|field| !field.skipped) {
        match object.get(&field.visible_name) {
            Some(value) => validate_field(field, value, defs)?,
            None if field.range.allows_missing() || field.default_value.is_some() => {}
            None => {
                return Err(syn::Error::new(
                    proc_macro2::Span::call_site(),
                    format!("example is missing required field `{}`", field.visible_name),
                ));
            }
        }
    }

    Ok(())
}

fn validate_field(
    field: &FieldDef,
    value: &Value,
    defs: &BTreeMap<String, ObjectDef>,
) -> Result<()> {
    if value.is_null() {
        if matches!(field.range, Range::ZeroToOne) {
            return Ok(());
        }
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("field `{}` does not allow null", field.visible_name),
        ));
    }

    match (&field.ty, value) {
        (FieldType::Primitive(expected), actual) => validate_primitive(field, *expected, actual),
        (FieldType::Object(name), Value::Object(object)) => {
            let def = defs.get(name).ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    format!("unknown nested object type `{name}`"),
                )
            })?;
            validate_object(def, object, defs)
        }
        (FieldType::Array(inner), Value::Array(items)) => {
            field.range.validate_len(items.len()).map_err(|message| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    format!("field `{}` {}", field.visible_name, message),
                )
            })?;
            for item in items {
                validate_inner_value(inner, item, defs, &field.visible_name)?;
            }
            Ok(())
        }
        (FieldType::Map(inner), Value::Object(object)) => {
            for value in object.values() {
                validate_inner_value(inner, value, defs, &field.visible_name)?;
            }
            Ok(())
        }
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "field `{}` has the wrong type; expected {}",
                field.visible_name,
                describe_type(&field.ty)
            ),
        )),
    }
}

fn validate_inner_value(
    ty: &FieldType,
    value: &Value,
    defs: &BTreeMap<String, ObjectDef>,
    field_name: &str,
) -> Result<()> {
    match (ty, value) {
        (FieldType::Primitive(expected), actual) => validate_primitive_name(field_name, *expected, actual),
        (FieldType::Object(name), Value::Object(object)) => {
            let def = defs.get(name).ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    format!("unknown nested object type `{name}`"),
                )
            })?;
            validate_object(def, object, defs)
        }
        (FieldType::Array(inner), Value::Array(values)) => {
            for value in values {
                validate_inner_value(inner, value, defs, field_name)?;
            }
            Ok(())
        }
        (FieldType::Map(inner), Value::Object(object)) => {
            for value in object.values() {
                validate_inner_value(inner, value, defs, field_name)?;
            }
            Ok(())
        }
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("field `{field_name}` has the wrong nested type; expected {}", describe_type(ty)),
        )),
    }
}

fn validate_primitive(field: &FieldDef, expected: PrimitiveType, actual: &Value) -> Result<()> {
    validate_primitive_name(&field.visible_name, expected, actual)
}

fn validate_primitive_name(field_name: &str, expected: PrimitiveType, actual: &Value) -> Result<()> {
    let ok = match (expected, actual) {
        (PrimitiveType::String, Value::String(_)) => true,
        (PrimitiveType::Integer, Value::Number(n)) => n.is_i64() || n.is_u64(),
        (PrimitiveType::Float, Value::Number(_)) => true,
        (PrimitiveType::Boolean, Value::Bool(_)) => true,
        _ => false,
    };

    if ok {
        return Ok(());
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        format!(
            "field `{field_name}` has the wrong type; expected {}",
            describe_type(&FieldType::Primitive(expected))
        ),
    ))
}

pub fn render_schema(schema: &ToolSchema) -> Result<String> {
    let defs = schema
        .types
        .iter()
        .map(|item| (item.def.name.clone(), item.def.clone()))
        .collect::<BTreeMap<_, _>>();
    let root = defs.get(&schema.root).ok_or_else(|| {
        syn::Error::new(proc_macro2::Span::call_site(), "missing root definition")
    })?;

    let mut lines = Vec::new();
    if let Some(description) = &schema.description {
        lines.push(format!("# {description}"));
    }
    lines.push(format!("tool: {}", schema.name));

    for field in root.fields.iter().filter(|field| !field.skipped) {
        if let Some(description) = &field.description {
            lines.push(format!("# {description}"));
        }
        let mut line = format!(
            "{}{}: {}",
            field.visible_name,
            field.range.format(),
            describe_type(&field.ty)
        );
        if let Some(default_value) = &field.default_value {
            line.push_str(&format!(" # default: {default_value}"));
        }
        lines.push(line);
    }

    lines.push(String::new());
    lines.push(String::from("Examples:"));

    for (index, example) in schema.examples.iter().enumerate() {
        lines.push(String::from("<tool_call>"));
        lines.push(format!("tool: {}", schema.name));

        let encoded = toon_format::encode_default(&example.value).map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("failed to encode example to TOON: {error}"),
            )
        })?;

        lines.extend(encoded.lines().map(ToOwned::to_owned));
        lines.push(String::from("</tool_call>"));
        if index + 1 != schema.examples.len() {
            lines.push(String::new());
        }
    }

    Ok(lines.join("\n"))
}

fn describe_type(ty: &FieldType) -> String {
    match ty {
        FieldType::Primitive(PrimitiveType::String) => String::from("string"),
        FieldType::Primitive(PrimitiveType::Integer) => String::from("integer"),
        FieldType::Primitive(PrimitiveType::Float) => String::from("float"),
        FieldType::Primitive(PrimitiveType::Boolean) => String::from("boolean"),
        FieldType::Object(_) => String::from("object"),
        FieldType::Array(inner) => format!("array<{}>", describe_type(inner)),
        FieldType::Map(inner) => format!("map<string, {}>", describe_type(inner)),
    }
}
