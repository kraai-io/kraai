use super::*;

fn model(max_context: Option<usize>, max_output: Option<usize>) -> crate::Model {
    crate::Model {
        id: kraai_types::ModelId::new("model"),
        name: String::from("model"),
        max_context,
        max_output,
        supports_images: false,
        options: Vec::new(),
    }
}

fn input(tokens: usize) -> Vec<ConversationItem> {
    let item = |text: String| ConversationItem::User {
        content: text.into(),
    };
    let overhead = item(String::new()).estimated_tokens();
    vec![item(
        "a".repeat((tokens - overhead) * kraai_types::BYTES_PER_TOKEN),
    )]
}

#[test]
fn responses_fill_the_context_up_to_the_compaction_point() {
    let messages = input(5_000);
    assert_eq!(compaction_point(16_000), 12_800);
    assert_eq!(
        model(Some(16_000), None).output_limit(&messages),
        Some(7_800)
    );
    assert_eq!(
        model(Some(16_000), Some(4_096)).output_limit(&messages),
        Some(4_096)
    );
    assert_eq!(
        model(None, Some(4_096)).output_limit(&messages),
        Some(4_096)
    );
    assert_eq!(model(None, None).output_limit(&messages), None);
}

#[test]
fn input_past_the_compaction_point_may_use_the_rest_of_the_window() {
    assert_eq!(
        model(Some(16_000), None).output_limit(&input(13_000)),
        Some(3_000)
    );
    assert_eq!(
        model(Some(16_000), None).output_limit(&input(12_800)),
        Some(3_200)
    );
    assert_eq!(
        model(Some(16_000), Some(8_192)).output_limit(&input(16_000)),
        None
    );
}

#[test]
fn only_a_limit_set_by_the_context_left_counts_as_context_bound() {
    let messages = input(5_000);
    assert!(model(Some(16_000), None).output_limited_by_context(&messages));
    assert!(model(Some(16_000), Some(8_192)).output_limited_by_context(&messages));
    assert!(!model(Some(16_000), Some(4_096)).output_limited_by_context(&messages));
    assert!(!model(None, Some(4_096)).output_limited_by_context(&messages));
    assert!(!model(None, None).output_limited_by_context(&messages));
}
