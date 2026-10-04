#![expect(
    clippy::panic,
    clippy::indexing_slicing,
    reason = "catalog tests assert rendering and schema boundaries"
)]

use super::*;

fn tool(server: &str, name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        server: server.into(),
        tool: rmcp::model::Tool::new(
            name.to_string(),
            description.to_string(),
            serde_json::Map::from_iter([
                (String::from("type"), json!("object")),
                (
                    String::from("properties"),
                    json!({"query": {"type": "string"}}),
                ),
                (String::from("required"), json!(["query"])),
            ]),
        ),
    }
}

#[test]
fn threshold_uses_utf8_bytes_of_the_complete_rendered_prompt() {
    let tools = [tool("issues", "search", "検索")];
    let servers = json!([{"server": "issues", "description": "Issues"}]);
    let Some(full) = render(&tools, servers.clone(), usize::MAX) else {
        panic!("missing prompt")
    };
    assert!(full.len() > full.chars().count());
    assert_eq!(
        render(&tools, servers.clone(), full.len()),
        Some(full.clone())
    );
    let Some(deferred) = render(&tools, servers, full.len() - 1) else {
        panic!("missing prompt")
    };
    assert!(deferred.contains("kraai-mcp search"));
    assert!(!deferred.contains("inputSchema"));
    assert!(full.contains("inputSchema"));
}

#[test]
fn search_returns_schemas_and_disambiguates_server_names() {
    let results = search(
        vec![
            tool("b", "search", "Find issues"),
            tool("a", "search", "Find issues"),
        ],
        "issues",
        1,
        vec![],
    );
    assert_eq!(results["tools"][0]["server"], "a");
    assert_eq!(
        results["tools"][0]["inputSchema"]["required"],
        json!(["query"])
    );
    assert_eq!(results["has_more"], true);
}

#[test]
fn empty_configuration_adds_no_prompt() {
    assert!(render(&[], json!([]), 0).is_none());
}
