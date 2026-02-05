use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub type ToolId = Arc<String>;

#[derive(Deserialize)]
#[serde(untagged)]
pub enum ToolOutput {
    Success {
        #[serde(flatten)]
        data: serde_json::Value,
    },
    Error {
        message: String,
    },
}

impl ToolOutput {
    pub fn error(message: String) -> Self {
        Self::Error { message }
    }

    pub fn success<D: Serialize>(data: D) -> Self {
        let data = serde_json::to_value(data).unwrap();
        Self::Success { data }
    }
}

#[async_trait]
pub trait Tool {
    fn name(&self) -> Arc<String>;

    fn description(&self) -> Arc<String>;

    fn parameters_schema(&self) -> Arc<String>;

    async fn call(&self, args: serde_json::Value) -> ToolOutput;
}

#[derive(Default)]
pub struct ToolManager {
    pub tools: BTreeMap<ToolId, Box<dyn Tool + Send + Sync>>,
}
