//! Intermediate representation for Toon schema generation

/// Schema for a struct
#[derive(Debug)]
pub struct Schema {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<Field>,
    pub examples: Vec<String>,
}

/// Field in a struct
#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub description: Option<String>,
    pub range: Range,
    pub skipped: bool,
    pub default_value: Option<String>, // JSON string or None
    pub optional: bool,
}

/// Type representation
#[derive(Debug)]
pub enum Type {
    Primitive(PrimitiveType),
    Array(Box<Type>),
}

/// Primitive types
#[derive(Debug)]
pub enum PrimitiveType {
    String,
    Integer,
    Float,
    Boolean,
}

/// Range notation [min:max]
#[derive(Debug)]
pub enum Range {
    Exactly(u32),      // [N:N] - required fields
    ZeroToOne,         // [0:1] - Option<T>
    ZeroOrMore,        // [0:] - Vec<T> without custom range
    AtLeast(u32),      // [N:] - Vec<T> with min=N
    Bounded(u32, u32), // [min:max] - Vec<T> with custom range
}

impl Range {
    /// Format the range as a string for display
    pub fn format(&self) -> String {
        match self {
            Range::Exactly(n) => format!("[{}:{}]", n, n),
            Range::ZeroToOne => "[0:1]".to_string(),
            Range::ZeroOrMore => "[0:]".to_string(),
            Range::AtLeast(n) => format!("[{}:]", n),
            Range::Bounded(min, max) => format!("[{}:{}]", min, max),
        }
    }
}
