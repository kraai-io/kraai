#[macro_use]
mod metadata;
mod commands;
#[cfg(feature = "nushell")]
mod nushell;

use std::collections::HashSet;

pub use commands::{close_files, edit_file, mcp, open_files, view_image, web_search};
pub use metadata::{CommandExample, CommandMetadata, HostService};
#[cfg(feature = "nushell")]
pub(crate) use nushell::declare_kraai_command;
#[cfg(feature = "nushell")]
pub use nushell::{
    CommandContext, CommandRegistration, CommandRegistry, CommandRegistryError,
    ImageAttachmentClient, McpClient, StateEffectClient, StateEffectError, WebSearchClient,
    command_error, command_registry,
};

static COMMANDS: [&CommandMetadata; 6] = [
    &mcp::METADATA,
    &open_files::METADATA,
    &close_files::METADATA,
    &edit_file::METADATA,
    &web_search::METADATA,
    &view_image::METADATA,
];

pub fn command_metadata(command_id: &str) -> Option<&'static CommandMetadata> {
    COMMANDS
        .iter()
        .copied()
        .find(|metadata| metadata.id == command_id)
}

pub fn command_ids() -> impl Iterator<Item = &'static str> {
    COMMANDS.iter().map(|metadata| metadata.id)
}

pub fn command_id_set() -> HashSet<String> {
    command_ids().map(String::from).collect()
}

pub fn provides_service(active_commands: &[String], service: HostService) -> bool {
    active_commands
        .iter()
        .filter_map(|id| command_metadata(id))
        .any(|metadata| metadata.services.contains(&service))
}

#[cfg(test)]
mod tests;
