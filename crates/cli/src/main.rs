use color_eyre::eyre::Result;
use futures::StreamExt;
use provider_core::ProviderManager;
use provider_google::GoogleFactory;
use types::{ChatMessage, ChatRole};

#[tokio::main]
async fn main() -> Result<()> {
    let mut manager = ProviderManager::new();
    manager.register_factory::<GoogleFactory>();
    let config_slice = std::fs::read("./crates/cli/config/config.toml")?;
    let config = toml::from_slice(&config_slice)?;
    manager.load_config(config).await?;

    let mut result = manager
        .generate_reply_stream(
            "google".to_string().into(),
            &"gemini-2.0-flash".to_string().into(),
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".to_string(),
            }],
        )
        .await?;
    while let Some(s) = result.next().await {
        println!("{:#?}", s?);
    }
    Ok(())
}
