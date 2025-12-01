use color_eyre::eyre::Result;
use provider_core::{ChatMessage, ChatRole, ProviderManager};
use provider_google::GoogleFactory;

#[tokio::main]
async fn main() -> Result<()> {
    let mut manager = ProviderManager::new();
    manager.register_factory::<GoogleFactory>();
    let config_slice = std::fs::read("./crates/cli/config/config.toml")?;
    let config = toml::from_slice(&config_slice)?;
    manager.load_config(config).await?;

    let result = manager
        .generate_reply(
            "google".to_string(),
            &"gemini-2.0-flash".to_string(),
            vec![ChatMessage {
                role: ChatRole::User,
                content: "hi".to_string(),
            }],
        )
        .await?;
    println!("{:#?}", result);
    Ok(())
}
