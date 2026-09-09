#![forbid(unsafe_code)]

use std::collections::HashSet;

use kraai_command_core::{CommandContext, CommandMetadata, CommandRegistry, CommandRegistryError};

static COMMANDS: [&CommandMetadata; 3] = [
    &kraai_command_open_files::OpenFilesCommand::METADATA,
    &kraai_command_close_files::CloseFilesCommand::METADATA,
    &kraai_command_edit_file::EditFileCommand::METADATA,
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

pub fn command_registry(context: CommandContext) -> Result<CommandRegistry, CommandRegistryError> {
    let open_files = kraai_command_open_files::OpenFilesCommand::registration(context.clone())?;
    let close_files = kraai_command_close_files::CloseFilesCommand::registration(context.clone())?;
    let edit_file = kraai_command_edit_file::EditFileCommand::registration(context)?;
    CommandRegistry::new([open_files, close_files, edit_file])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ids_are_unique_and_resolve_to_their_metadata() {
        let ids = command_id_set();
        assert_eq!(ids.len(), COMMANDS.len());
        for id in command_ids() {
            assert_eq!(command_metadata(id).map(|metadata| metadata.id), Some(id));
        }
    }
}
