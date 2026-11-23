use super::*;

#[test]
fn opened_file_effects_keep_the_generic_wire_contract() {
    for (operation, wire_operation) in [
        (OpenedFilesOperation::Open, "open"),
        (OpenedFilesOperation::Close, "close"),
    ] {
        let delta = operation.into_delta("/workspace/🦀.rs");
        let expected = serde_json::json!({
            "namespace": "opened_files",
            "operation": wire_operation,
            "payload": { "path": "/workspace/🦀.rs" },
        });
        let encoded = serde_json::to_value(&delta).unwrap();
        assert_eq!(encoded, expected);
        assert_eq!(delta.opened_file_path(), Some("/workspace/🦀.rs"));
        assert_eq!(
            OpenedFilesOperation::parse(&delta.operation),
            Some(operation)
        );
    }
}

#[test]
fn generic_effects_preserve_unknown_operations_and_payload_fields() {
    let value = serde_json::json!({
        "namespace": "opened_files",
        "operation": "future-operation",
        "payload": { "path": "/workspace/file", "extra": true },
        "extra": "ignored",
    });
    let delta: ContextStateDelta = serde_json::from_value(value).unwrap();
    assert_eq!(delta.opened_file_path(), Some("/workspace/file"));
    assert_eq!(OpenedFilesOperation::parse(&delta.operation), None);
    assert_eq!(
        delta.payload.get("extra"),
        Some(&serde_json::Value::Bool(true))
    );
}
