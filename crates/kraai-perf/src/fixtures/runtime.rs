use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, eyre};
use kraai_provider_core::{ProviderSet, ProviderSetup};
use kraai_runtime::{RuntimeBuilder, RuntimeHandle};

use super::provider::{self, FixtureFactory};

pub(crate) const SOURCES: &[&[u8]] = &[include_bytes!("runtime.rs"), include_bytes!("provider.rs")];

struct Providers;

#[async_trait::async_trait]
impl ProviderSet for Providers {
    async fn load(&self, storage_root: &Path) -> Result<ProviderSetup> {
        let mut setup = kraai_providers::DefaultProviders.load(storage_root).await?;
        setup
            .registry
            .register_factory::<FixtureFactory>()
            .map_err(|error| eyre!(error.to_string()))?;
        Ok(setup)
    }
}

pub(crate) fn prepare(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace(directory))?;
    std::fs::create_dir_all(storage(directory))?;
    std::fs::write(
        directory.join("providers.toml"),
        format!(
            "[[provider]]\nid = \"{}\"\ntype = \"{}\"\n",
            provider::provider_id(),
            <FixtureFactory as kraai_provider_core::ProviderFactory>::TYPE_ID,
        ),
    )?;
    Ok(())
}

pub(crate) fn start(directory: &Path) -> RuntimeHandle {
    RuntimeBuilder::new(Providers)
        .storage_root(storage(directory))
        .provider_config_path(directory.join("providers.toml"))
        .mcp_config_path(directory.join("mcp.toml"))
        .build_on(&tokio::runtime::Handle::current())
}

pub(crate) fn workspace(directory: &Path) -> PathBuf {
    directory.join("workspace")
}

fn storage(directory: &Path) -> PathBuf {
    directory.join("storage")
}
