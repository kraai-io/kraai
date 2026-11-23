use super::*;

#[test]
fn estimates_round_up_and_count_every_part() {
    assert_eq!(estimate_text_tokens(""), 0);
    assert_eq!(estimate_text_tokens("abcde"), 2);
    let user = ConversationItem::User {
        content: MessageContent(vec![
            ContentPart::Text {
                text: "x".repeat(400),
            },
            ContentPart::Image {
                image: kraai_execution_types::ImageAttachment {
                    id: "a".repeat(64),
                    mime_type: "image/png".into(),
                    width: 2,
                    height: 3,
                    byte_length: 100,
                },
            },
        ]),
    };
    assert_eq!(
        user.estimated_tokens(),
        ITEM_OVERHEAD_TOKENS + 100 + IMAGE_TOKENS
    );
}
