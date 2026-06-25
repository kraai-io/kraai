use serde::{Deserialize, Serialize};

use crate::config::DynamicValue;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldValueKind {
    String,
    SecretString,
    Boolean,
    Integer,
    Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub key: String,
    pub label: String,
    pub value_kind: FieldValueKind,
    pub required: bool,
    pub secret: bool,
    pub help_text: Option<String>,
    pub default_value: Option<DynamicValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDefinition {
    pub type_id: String,
    pub display_name: String,
    pub protocol_family: String,
    pub description: String,
    pub provider_fields: Vec<FieldDefinition>,
    pub model_fields: Vec<FieldDefinition>,
    pub supports_model_discovery: bool,
    pub default_provider_id_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}
