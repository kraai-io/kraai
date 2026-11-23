use super::*;

#[test]
fn transcript_separates_items_but_not_their_deltas() {
    let mut transcript = Transcript::default();
    let mut output = Vec::new();
    let text = |text: &str| AssistantItem::Text {
        phase: kraai_types::AssistantPhase::FinalAnswer,
        text: text.into(),
    };
    transcript
        .item(&mut output, "m", 0, &text("Looking"))
        .unwrap();
    transcript.text(&mut output, "m", 0, " closer.").unwrap();
    transcript
        .item(
            &mut output,
            "m",
            1,
            &AssistantItem::ScriptCall(ScriptCall {
                call_id: kraai_types::ToolCallId::new("call"),
                name: "kraai_nushell".into(),
                input: "ls".into(),
                arguments_error: None,
                rewrite: None,
            }),
        )
        .unwrap();
    transcript
        .item(&mut output, "n", 0, &text("Done."))
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output),
        "Looking closer.\n\n```nu\nls\n```\nDone."
    );
}
