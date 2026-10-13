use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchRequest {
    pub query: String,
    pub limit: usize,
    pub max_chars: usize,
}

impl WebSearchRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.query.trim().is_empty() || self.query.len() > 4096 {
            return Err(String::from(
                "query must contain 1 to 4096 bytes of nonempty text",
            ));
        }
        if !(1..=10).contains(&self.limit) {
            return Err(String::from("limit must be between 1 and 10"));
        }
        if !(1..=20000).contains(&self.max_chars) {
            return Err(String::from("max-chars must be between 1 and 20000"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchResponse {
    pub provider: String,
    pub content: String,
    pub truncated: bool,
}
