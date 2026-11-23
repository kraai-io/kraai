use super::*;

#[test]
fn command_ids_are_unique_and_resolve_to_their_metadata() {
    let ids = command_id_set();
    assert_eq!(ids.len(), COMMANDS.len());
    for id in command_ids() {
        assert_eq!(command_metadata(id).map(|metadata| metadata.id), Some(id));
    }
}

#[test]
fn services_are_granted_only_by_declaring_commands() {
    let active = [String::from(view_image::METADATA.id)];
    assert!(provides_service(&active, HostService::Images));
    assert!(!provides_service(&active, HostService::Mcp));
    assert!(!provides_service(&[], HostService::Images));
    assert!(!provides_service(
        &[String::from("unknown")],
        HostService::Images
    ));
}
