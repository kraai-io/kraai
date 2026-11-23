use super::*;

fn item(id: usize) -> ResponsesRequestItem {
    ResponsesRequestItem::Reasoning(
        serde_json::json!({"id": id, "encrypted_content": format!("opaque-{id}")}),
    )
}
#[test]
fn rejection_memory_is_bounded_and_preserves_new_reasoning() {
    let mut rejected = RejectedReasoning::default();
    for id in 0..=CAPACITY {
        rejected.reject(
            &[item(id)],
            r#"{"error":{"param":"input[0].encrypted_content"}}"#,
        );
    }
    assert_eq!(rejected.fingerprints.len(), CAPACITY);
    assert_eq!(rejected.order.len(), CAPACITY);
    let mut items = vec![item(CAPACITY), item(CAPACITY + 1)];
    rejected.filter(&mut items);
    assert_eq!(items.len(), 1);
    assert_eq!(
        items.first().and_then(fingerprint),
        fingerprint(&item(CAPACITY + 1))
    );
}

#[test]
fn only_unambiguously_identified_reasoning_is_suppressed() {
    for response in [
        r#"{"error":{"message":"The encrypted content opaque-bad could not be verified."}}"#,
        r#"{"error":{"message":"The encrypted content for item rs-bad could not be verified."}}"#,
        r#"{"error":{"param":"input[1].encrypted_content"}}"#,
    ] {
        let mut items = vec![
            ResponsesRequestItem::Reasoning(
                serde_json::json!({"id":"rs-good","encrypted_content":"opaque-good"}),
            ),
            ResponsesRequestItem::Reasoning(
                serde_json::json!({"id":"rs-bad","encrypted_content":"opaque-bad"}),
            ),
        ];
        let mut cache = RejectedReasoning::default();
        cache.reject(&items, response);
        cache.filter(&mut items);
        assert_eq!(items.len(), 1);
        assert!(
            matches!(items.first(), Some(ResponsesRequestItem::Reasoning(payload)) if payload["id"] == "rs-good")
        );
    }
    for message in [
        "could not verify content",
        "opaque-...",
        "rs-good rs-bad",
        "rs-bad-extra",
    ] {
        let mut items = vec![
            ResponsesRequestItem::Reasoning(
                serde_json::json!({"id":"rs-good","encrypted_content":"opaque-good"}),
            ),
            ResponsesRequestItem::Reasoning(
                serde_json::json!({"id":"rs-bad","encrypted_content":"opaque-bad"}),
            ),
        ];
        let mut cache = RejectedReasoning::default();
        cache.reject(
            &items,
            &serde_json::json!({"error":{"message":message}}).to_string(),
        );
        cache.filter(&mut items);
        assert_eq!(items.len(), 2);
    }
}
