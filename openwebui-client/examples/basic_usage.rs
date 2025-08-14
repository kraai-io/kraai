use openwebui_client::{ChatCompletionRequest, ChatMessage, ChatRole, OpenWebUIClient};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Open WebUI API Client - Basic Usage Examples");
    println!("============================================\n");

    // Create a client
    let client = OpenWebUIClient::new("http://localhost:3000");

    // Example 1: List available models
    println!("1. Listing available models...");
    match client.list_models().await {
        Ok(models) => {
            println!("   Found {} models:", models.len());
            for model in models.iter().take(3) {
                // Show first 3 models
                println!("   - {}", model.id);
            }
            if models.len() > 3 {
                println!("   ... and {} more", models.len() - 3);
            }
        }
        Err(e) => println!("   Error: {}", e),
    }

    // Example 2: Simple chat
    println!("\n2. Simple chat example...");
    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: "What is the capital of Japan?".to_string(),
    }];

    match client.simple_chat("gemma3:4b", messages).await {
        Ok(response) => println!("   AI Response: {}", response),
        Err(e) => println!("   Error: {}", e),
    }

    // Example 3: Chat with system message
    println!("\n3. Chat with system message...");
    match client
        .chat_with_system(
            "gemma3:4b",
            "You are a helpful math tutor. Keep your answers concise and clear.",
            "What is the derivative of x²?",
        )
        .await
    {
        Ok(response) => println!("   AI Response: {}", response),
        Err(e) => println!("   Error: {}", e),
    }

    // Example 4: Custom chat completion with parameters
    println!("\n4. Custom chat completion...");
    let custom_messages = vec![
        ChatMessage {
            role: ChatRole::System,
            content: "You are a creative writer. Write in a poetic style.".to_string(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: "Describe a sunset in one sentence.".to_string(),
        },
    ];

    let request = ChatCompletionRequest {
        model: "gemma3:4b".to_string(),
        messages: custom_messages,
        temperature: Some(0.9), // More creative
        max_tokens: Some(200),  // Shorter response
        stream: Some(false),
    };

    match client.chat_completion(request).await {
        Ok(response) => {
            if let Some(choice) = response.choices.first() {
                println!("   AI Response: {}", choice.message.content);
                println!("   Model used: {}", response.model);
                println!("   Total tokens: {}", response.usage.total_tokens);
            }
        }
        Err(e) => println!("   Error: {}", e),
    }

    println!("\nExamples completed!");
    Ok(())
}
