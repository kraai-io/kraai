mod tools;

use anyhow::Result;
use openwebui_client::{ChatCompletionRequest, ChatMessage, ChatRole, OpenWebUIClient};

struct AgentConfig {
    pub base_path: String,
}

struct Agent {
    config: AgentConfig,
    messages: Vec<ChatMessage>,
    client: OpenWebUIClient,
}

impl Agent {
    pub fn new(config: AgentConfig) -> Self {
        let client = OpenWebUIClient::with_api_key(
            std::env::var("OPEN_WEBUI_HOST").unwrap(),
            std::env::var("OPEN_WEBUI_API_KEY").unwrap(),
        );

        Self {
            config,
            messages: vec![],
            client,
        }
    }

    fn system_prompt(&self) -> String {
        tools::get_tool_prompts(&self.config)
    }

    pub async fn send_user_message(&mut self, message: impl Into<String>) {
        let mut messages = vec![ChatMessage {
            role: ChatRole::System,
            content: self.system_prompt(),
        }];
        messages.append(&mut self.messages);
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: message.into(),
        });

        let request = ChatCompletionRequest {
            model: "gemma3:4b".to_string(),
            // model: "qwen3:4b".to_string(),
            // model: "models/gemini-2.5-flash".to_string(),
            messages: messages.clone(),
            temperature: None,
            max_tokens: None,
            stream: Some(false),
        };

        let mut tool_called = false;

        match self.client.chat_completion(request).await {
            Ok(response) => {
                if let Some(choice) = response.choices.first() {
                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: choice.message.content.clone(),
                    });
                    let tools = tools::parse_tool_calls(&choice.message.content).unwrap();
                    tool_called = !tools.is_empty();
                    for (tool, params) in tools {
                        // TODO: request tool call activation from user
                        let res = tools::call_tool(&self.config, tool, params).await.unwrap();
                        messages.push(ChatMessage {
                            role: ChatRole::User,
                            content: res,
                        });
                    }
                }
            }
            Err(e) => println!("Chat completion failed: {}", e),
        }

        messages.remove(0); // remove system prompt
        self.messages = messages;
        if tool_called {
            self.send_after_tool_calls().await;
        }
    }

    async fn send_after_tool_calls(&mut self) {
        let mut messages = vec![ChatMessage {
            role: ChatRole::System,
            content: self.system_prompt(),
        }];
        messages.append(&mut self.messages);

        let request = ChatCompletionRequest {
            model: "gemma3:4b".to_string(),
            // model: "qwen3:4b".to_string(),
            // model: "models/gemini-2.5-flash".to_string(),
            messages: messages.clone(),
            temperature: None,
            max_tokens: None,
            stream: Some(false),
        };

        let mut tool_called = false;

        match self.client.chat_completion(request).await {
            Ok(response) => {
                if let Some(choice) = response.choices.first() {
                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: choice.message.content.clone(),
                    });
                    let tools = tools::parse_tool_calls(&choice.message.content).unwrap();
                    tool_called = !tools.is_empty();
                    for (tool, params) in tools {
                        // TODO: request tool call activation from user
                        let res = tools::call_tool(&self.config, tool, params).await.unwrap();
                        messages.push(ChatMessage {
                            role: ChatRole::User,
                            content: res,
                        });
                    }
                }
            }
            Err(e) => println!("Chat completion failed: {}", e),
        }

        messages.remove(0); // remove system prompt
        self.messages = messages;
        if tool_called {
            Box::pin(self.send_after_tool_calls()).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    println!("AI Agent Start");
    println!("==================================\n");

    let agent_config = AgentConfig {
        base_path: "C:/Users/ominit/Desktop/code/ai-agent/".to_string(),
    };

    let mut agent = Agent::new(agent_config);

    agent
        .send_user_message("There is a rust project at `ai-agent/`. Using what you know about rust projects, read the rest of the files in the project and then give me a summary of what the project is about.")
        .await;
    for m in agent.messages {
        println!("===============");
        println!("{}", m);
    }
    println!("===============");

    println!("\n==================================");
    println!("AI Agent End");
    Ok(())
}
