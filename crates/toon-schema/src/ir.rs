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
}

/// Type representation
pub enum Type {
    Primitive(PrimitiveType),
    Array(Box<Type>),
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
    Exactly(u32), // [N:N] - required fields
    ZeroToOne,    // [0:1] - Option<T>
    ZeroOrMore,   // [0:] - Vec<T>
}
