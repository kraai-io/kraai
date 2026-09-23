#![forbid(unsafe_code)]

mod builtins;

use std::collections::HashSet;

use kraai_types::CommandMetadata;

pub use builtins::{CLOSE_FILES, EDIT_FILE, OPEN_FILES, WEB_SEARCH};

static COMMANDS: [&CommandMetadata; 4] = [&OPEN_FILES, &CLOSE_FILES, &EDIT_FILE, &WEB_SEARCH];

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
