use std::{convert::Infallible, time::Duration};

use axum::{
    Extension, Json, Router,
    response::{Sse, sse::Event},
    routing::post,
};
use futures::{SinkExt, Stream, StreamExt};
use llm_api_core::{ChatMessage, ChatRole, ClientId, LLMManager, ModelId};
use llm_api_openwebui::OpenWebuiClient;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let mut manager = LLMManager::new();

    let openwebui_client = OpenWebuiClient::with_api_key(
        &std::env::var("OPEN_WEBUI_HOST").unwrap(),
        &std::env::var("OPEN_WEBUI_API_KEY").unwrap(),
    );

    manager.add_client(openwebui_client).await.unwrap();

    let router = Router::new()
        .route("/api/generate", post(handle_generate))
        .route("/api/generate-stream", post(handle_stream))
        .layer(Extension(manager));

    let listener = TcpListener::bind("0.0.0.0:8080").await.unwrap();

    axum::serve(listener, router.into_make_service())
        .await
        .unwrap();
}

#[axum::debug_handler]
async fn handle_generate(
    Extension(manager): Extension<LLMManager>,
    Json(payload): Json<LLMRequest>,
) -> Json<LLMResponse> {
    Json(generate_response(&manager, payload).await)
}

async fn handle_stream(
    Extension(manager): Extension<LLMManager>,
    Json(payload): Json<LLMRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // Create channel for async communication
    let (mut tx, rx) = futures::channel::mpsc::channel(100);

    tokio::spawn(async move {
        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: payload.system_prompt,
            },
            ChatMessage {
                role: ChatRole::User,
                content: payload.user_message,
            },
        ];

        // Critical: Use the stream method
        match manager
            .generate_reply_stream(payload.client.into(), payload.model.into(), messages)
            .await
        {
            Ok(mut stream) => {
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(content) if !content.is_empty() => {
                            let _ = tx.send(Ok(Event::default().data(content))).await;
                        }
                        Ok(_) => {
                            // [DONE] event
                            let _ = tx.send(Ok(Event::default().event("complete"))).await;
                            break;
                        }
                        Err(e) => {
                            let _ = tx
                                .send(Ok(Event::default().event("error").data(e.to_string())))
                                .await;
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let _ = tx
                    .send(Ok(Event::default()
                        .event("error")
                        .data(format!("Stream failed: {}", e))))
                    .await;
            }
        }
    });

    // Convert to SSE stream
    Sse::new(rx).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive-text"),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMRequest {
    pub client: ClientId,
    pub model: ModelId,
    pub system_prompt: String,
    pub user_message: String,
}

#[derive(Debug, Serialize)]
pub struct LLMResponse {
    pub content: String,
    pub tokens_used: u32,
}

pub async fn generate_response(manager: &LLMManager, req: LLMRequest) -> LLMResponse {
    let messages = vec![
        ChatMessage {
            role: ChatRole::System,
            content: req.system_prompt,
        },
        ChatMessage {
            role: ChatRole::User,
            content: req.user_message,
        },
    ];

    // Critical: Test your core library
    let response = manager
        .generate_reply(req.client.into(), req.model.into(), messages)
        .await
        .unwrap();

    LLMResponse {
        content: response.content,
        tokens_used: 152, // Placeholder - you'll add actual counting later
    }
}
