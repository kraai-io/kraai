use std::collections::BTreeMap;

use kraai_types::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DynamicValue {
    String(String),
    Bool(bool),
    Integer(i64),
}

impl DynamicValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Bool(_) | Self::Integer(_) => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::String(_) | Self::Integer(_) => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            Self::String(_) | Self::Bool(_) => None,
        }
    }
}

impl From<String> for DynamicValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for DynamicValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<bool> for DynamicValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for DynamicValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

pub type DynamicConfig = BTreeMap<String, DynamicValue>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderManagerConfig {
    #[serde(default, rename = "provider")]
    pub providers: Vec<ProviderConfig>,
    #[serde(default, rename = "model")]
    pub models: Vec<ModelConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: ModelId,
    pub provider_id: ProviderId,
    #[serde(flatten)]
    pub config: DynamicConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: ProviderId,
    #[serde(rename = "type")]
    pub type_id: String,
    #[serde(flatten)]
    pub config: DynamicConfig,
}
