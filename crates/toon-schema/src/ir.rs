//! Intermediate representation for the `toon_tool!` proc macro.
use serde_json::Value;
use syn::ItemStruct;

#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: Option<String>,
    pub root: String,
    pub types: Vec<TypeItem>,
    pub examples: Vec<ExampleObject>,
}

#[derive(Debug, Clone)]
pub struct TypeItem {
    pub item: ItemStruct,
    pub def: ObjectDef,
}

#[derive(Debug, Clone)]
pub struct ObjectDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub visible_name: String,
    pub description: Option<String>,
    pub ty: FieldType,
    pub range: Range,
    pub default_value: Option<String>,
    pub skipped: bool,
}

#[derive(Debug, Clone)]
pub enum FieldType {
    Primitive(PrimitiveType),
    Object(String),
    Array(Box<FieldType>),
    Map(Box<FieldType>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveType {
    String,
    Integer,
    Float,
    Boolean,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Exactly(u32),
    ZeroToOne,
    ZeroOrMore,
    AtLeast(u32),
    Bounded(u32, u32),
}

impl Range {
    pub fn format(self) -> String {
        match self {
            Self::Exactly(n) => format!("[{n}:{n}]"),
            Self::ZeroToOne => String::from("[0:1]"),
            Self::ZeroOrMore => String::from("[0:]"),
            Self::AtLeast(n) => format!("[{n}:]"),
            Self::Bounded(min, max) => format!("[{min}:{max}]"),
        }
    }

    pub fn allows_missing(self) -> bool {
        matches!(
            self,
            Self::ZeroToOne | Self::ZeroOrMore | Self::AtLeast(0) | Self::Bounded(0, _)
        )
    }

    pub fn validate_len(self, len: usize) -> Result<(), String> {
        match self {
            Self::Exactly(n) if len != n as usize => {
                Err(format!("expected exactly {n} item(s), found {len}"))
            }
            Self::ZeroToOne if len > 1 => Err(format!("expected at most 1 item, found {len}")),
            Self::ZeroOrMore => Ok(()),
            Self::AtLeast(n) if len < n as usize => {
                Err(format!("expected at least {n} item(s), found {len}"))
            }
            Self::Bounded(min, max) if len < min as usize || len > max as usize => Err(format!(
                "expected between {min} and {max} item(s), found {len}"
            )),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExampleObject {
    pub value: Value,
}
