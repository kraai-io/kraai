use kraai_command_core::{CommandContext, CommandRegistry, CommandRegistryError};

pub(crate) fn command_registry(
    context: CommandContext,
) -> Result<CommandRegistry, CommandRegistryError> {
    let open_files = kraai_command_open_files::OpenFilesCommand::registration(context.clone())?;
    let close_files = kraai_command_close_files::CloseFilesCommand::registration(context.clone())?;
    let edit_file = kraai_command_edit_file::EditFileCommand::registration(context)?;
    CommandRegistry::new([open_files, close_files, edit_file])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use kraai_command_core::{StateEffectClient, StateEffectError};
    use kraai_types::ContextStateDelta;

    use super::*;

    struct UnusedEffects;

    impl StateEffectClient for UnusedEffects {
        fn apply(
            &self,
            _command_id: &'static str,
            _deltas: Vec<ContextStateDelta>,
        ) -> Result<(), StateEffectError> {
            Err(StateEffectError::new("test commands must not run"))
        }
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "catalog consistency test propagates setup errors and asserts the command contract"
    )]
    fn executable_commands_match_the_prompt_catalog() -> Result<(), Box<dyn std::error::Error>> {
        let registry = command_registry(CommandContext::new(Arc::new(UnusedEffects)))?;
        assert_eq!(
            registry.command_ids().collect::<BTreeSet<_>>(),
            kraai_command_catalog::command_ids().collect::<BTreeSet<_>>()
        );
        for id in registry.command_ids() {
            let metadata = kraai_command_catalog::command_metadata(id)
                .ok_or("registered command is missing metadata")?;
            for command in registry.select(&[String::from(id)])? {
                assert_eq!(command.name(), metadata.name);
                assert_eq!(command.signature().name, metadata.name);
                assert_eq!(command.description(), metadata.description);
                let examples = command.examples();
                assert_eq!(examples.len(), metadata.examples.len());
                for (example, metadata) in examples.iter().zip(metadata.examples) {
                    assert_eq!(example.description, metadata.description);
                    assert_eq!(example.example, metadata.script);
                    assert_eq!(
                        metadata
                            .script_input
                            .split_once('\n')
                            .map(|(_, script)| script),
                        Some(metadata.script)
                    );
                }
            }
        }
        Ok(())
    }
}
