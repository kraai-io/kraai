use super::*;

#[test]
fn executable_commands_match_the_prompt_catalog() {
    let registry = command_registry(CommandContext::parse_only()).unwrap();
    assert_eq!(
        registry.command_ids().collect::<BTreeSet<_>>(),
        crate::command_ids().collect::<BTreeSet<_>>()
    );
    for id in registry.command_ids() {
        let metadata = crate::command_metadata(id).expect("registered command is missing metadata");
        for command in registry.select(&[String::from(id)]).unwrap() {
            assert_eq!(command.name(), metadata.name);
            assert_eq!(command.signature().name, metadata.name);
            assert_eq!(command.description(), metadata.description);
            let examples = command.examples();
            assert_eq!(examples.len(), metadata.examples.len());
            for (example, metadata) in examples.iter().zip(metadata.examples) {
                assert_eq!(example.description, metadata.description);
                assert_eq!(example.example, metadata.script);
                assert!(metadata.script_input.ends_with(metadata.script));
            }
        }
    }
}
