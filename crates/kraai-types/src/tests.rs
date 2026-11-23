use super::*;

#[test]
fn assistant_rendering_preserves_empty_items_whitespace_and_tool_envelopes() {
    let text = |text: &str| AssistantItem::Text {
        phase: AssistantPhase::Commentary,
        text: text.to_string(),
    };
    let script = |input: &str| {
        AssistantItem::ScriptCall(ScriptCall {
            call_id: ToolCallId::new("call"),
            name: String::from("kraai_nushell"),
            input: input.to_string(),
            arguments_error: None,
            rewrite: None,
        })
    };
    for (items, expected) in [
        (Vec::new(), ""),
        (vec![text(""), text("")], ""),
        (vec![text(""), text(" "), text(""), text("é")], " \n\né"),
        (
            vec![text(""), script(""), text("")],
            "<tool_call>\n\n</tool_call>",
        ),
        (
            vec![
                text("é\n"),
                text(""),
                script("echo hi"),
                script("ls"),
                text(""),
            ],
            "é\n\n\n<tool_call>\necho hi\n</tool_call>\n\n<tool_call>\nls\n</tool_call>",
        ),
    ] {
        assert_eq!(
            ConversationItem::Assistant { items }.display_text(),
            expected
        );
    }
}

#[test]
fn display_text_borrows_existing_text() {
    let text = "é\n display";
    for content in [
        ConversationItem::System { text: text.into() },
        ConversationItem::User {
            content: text.into(),
        },
        ConversationItem::ScriptResult {
            outcome: kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: ToolCallId::new("call"),
            output: text.into(),
        },
        ConversationItem::Assistant {
            items: ["", text, ""]
                .into_iter()
                .map(|text| AssistantItem::Text {
                    phase: AssistantPhase::Commentary,
                    text: text.into(),
                })
                .collect(),
        },
    ] {
        assert!(matches!(content.display_text(), Cow::Borrowed(value) if value == text));
    }
}
