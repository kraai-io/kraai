use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum McpRequest {
    Servers,
    Tools {
        server: String,
    },
    Describe {
        server: String,
        tool: String,
    },
    Search {
        query: String,
        limit: usize,
    },
    Call {
        server: String,
        tool: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    },
}

impl McpRequest {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Servers => Ok(()),
            Self::Tools { server } => nonempty(server, "server"),
            Self::Describe { server, tool } | Self::Call { server, tool, .. } => {
                nonempty(server, "server")?;
                nonempty(tool, "tool")
            }
            Self::Search { query, limit } => {
                nonempty(query, "query")?;
                if !(1..=20).contains(limit) {
                    return Err(String::from("search limit must be between 1 and 20"));
                }
                Ok(())
            }
        }
    }
}

fn nonempty(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{name} must not be empty"))
    } else {
        Ok(())
    }
}
