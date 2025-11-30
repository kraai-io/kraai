use color_eyre::eyre::Result;
use provider_core::ProviderManager;
use provider_google::GoogleFactory;

#[tokio::main]
async fn main() -> Result<()> {
    let mut manager = ProviderManager::new();
    manager.register_factory::<GoogleFactory>();
    let config_slice = std::fs::read("./crates/cli/config/config.toml")?;
    let config = toml::from_slice(&config_slice)?;
    manager.load_config(config).await?;

    println!("{:#?}", manager.list_all_models());

    let result = manager
        .generate_reply("google".to_string(), &"gemini".to_string(), vec![])
        .await?;
    println!("{:#?}", result);
    Ok(())
}
