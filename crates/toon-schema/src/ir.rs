//! Intermediate representation for Toon schema generation

/// Schema for a struct
pub struct Schema {
    pub name: String,
    pub description: Option<String>,
    pub fields: Vec<Field>,
}

/// Field in a struct
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub description: Option<String>,
    pub example: String, // JSON string
    pub range: Range,
    pub skipped: bool,
    pub default_value: Option<String>, // JSON string or None
}

/// Type representation
pub enum Type {
    Primitive(PrimitiveType),
    Array(Box<Type>),
    Enum(EnumType),
}

/// Enum type with its variants
pub struct EnumType {
    pub variants: Vec<String>,
}

/// Primitive types
pub enum PrimitiveType {
    String,
    Integer,
    Float,
    Boolean,
}

/// Range notation [min:max]
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
