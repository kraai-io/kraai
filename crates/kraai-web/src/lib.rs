#![forbid(unsafe_code)]

mod response;

use std::sync::OnceLock;
use std::time::Duration;

use kraai_types::{WebSearchRequest, WebSearchResponse};

const ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const MAX_BYTES: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(25);

#[async_trait::async_trait]
pub trait WebSearch: Send + Sync {
    async fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResponse, String>;
}

#[derive(Default)]
pub struct ExaSearch {
    client: OnceLock<Result<reqwest::Client, String>>,
}

#[async_trait::async_trait]
impl WebSearch for ExaSearch {
    async fn search(&self, request: &WebSearchRequest) -> Result<WebSearchResponse, String> {
        request.validate()?;
        let client = self
            .client
            .get_or_init(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .connect_timeout(Duration::from_secs(10))
                    .timeout(DEADLINE)
                    .user_agent("kraai/0.1")
                    .build()
                    .map_err(|error| format!("web search client: {error}"))
            })
            .as_ref()
            .map_err(Clone::clone)?;
        tokio::time::timeout(DEADLINE, search(client, ENDPOINT, request))
            .await
            .map_err(|_error| String::from("web search timed out"))?
    }
}

async fn search(
    client: &reqwest::Client,
    endpoint: &str,
    request: &WebSearchRequest,
) -> Result<WebSearchResponse, String> {
    let mut response = client
        .post(endpoint)
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": request.query, "type": "auto", "numResults": request.limit,
                    "livecrawl": "fallback", "contextMaxCharacters": request.max_chars
                }
            }
        }))
        .send()
        .await
        .map_err(transport_error)?;
    match response.status().as_u16() {
        429 => return Err(String::from("web search rate limited; try again later")),
        401 | 403 => return Err(String::from("web search service rejected anonymous access")),
        200..=299 => {}
        status => return Err(format!("web search HTTP error: {status}")),
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Err(String::from("web search response exceeds 1 MiB"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > MAX_BYTES.saturating_sub(bytes.len()) {
            return Err(String::from("web search response exceeds 1 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = std::str::from_utf8(&bytes)
        .map_err(|_error| String::from("web search response is not UTF-8"))?;
    response::decode(body, request.max_chars)
}

fn transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        String::from("web search timed out")
    } else {
        format!("web search transport error: {error}")
    }
}

#[cfg(test)]
mod tests;
