use super::*;
use kraai_types::{AssistantItem, AssistantPhase, ConversationItem, MessageId, ToolCallId};

fn message(id: &str, role: ChatRole, content: &str) -> Message {
    let content = match role {
        ChatRole::System => ConversationItem::System {
            text: content.to_string(),
        },
        ChatRole::User => ConversationItem::User {
            content: content.into(),
        },
        ChatRole::Assistant => ConversationItem::Assistant {
            items: vec![AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: content.to_string(),
            }],
        },
        ChatRole::ToolCallResult => ConversationItem::ScriptResult {
            outcome: kraai_execution_types::ScriptExecutionOutcome {
                status: kraai_execution_types::ScriptExecutionStatus::Completed,
                exit_code: Some(0),
            },
            call_id: ToolCallId::new("test-call"),
            output: content.into(),
        },
    };
    Message {
        id: MessageId::new(id),
        parent_id: None,
        content,
        status: MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    }
}

fn script_message(before: &str, input: &str, after: &str) -> Message {
    let mut message = message("1", ChatRole::Assistant, "");
    message.content = ConversationItem::Assistant {
        items: vec![
            AssistantItem::Text {
                phase: AssistantPhase::Commentary,
                text: before.into(),
            },
            AssistantItem::ScriptCall(ScriptCall {
                call_id: ToolCallId::new("call"),
                name: "kraai_nushell".into(),
                input: input.into(),
                arguments_error: None,
                rewrite: None,
            }),
            AssistantItem::Text {
                phase: AssistantPhase::FinalAnswer,
                text: after.into(),
            },
        ],
    };
    message
}

#[test]
fn wraps_unicode_without_panicking() {
    let wrapped = ChatHistory::wrap_with_prefix("你好你好", 4, "", "");
    assert_eq!(wrapped, ["你好", "你好"]);
    assert!(wrapped.iter().all(|line| display_width(line) <= 4));
}

#[test]
fn wraps_styled_graphemes_across_span_and_line_boundaries() {
    let plain = Style::default();
    let emphasis = plain.fg(Color::Yellow);
    let spans = [
        RenderedSpan {
            text: String::from("ab"),
            style: plain,
        },
        RenderedSpan {
            text: String::new(),
            style: emphasis,
        },
        RenderedSpan {
            text: String::from("你e\u{301}"),
            style: emphasis,
        },
        RenderedSpan {
            text: String::from("👩‍💻z"),
            style: plain,
        },
    ];
    let mut lines = Vec::new();
    ChatHistory::push_wrapped_spans(&mut lines, &spans, 4, plain, ">", " ");
    assert_eq!(
        lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>(),
        [">ab", " 你e\u{301}", " 👩‍💻z"],
    );
    assert_eq!(lines[1].spans[1].style, emphasis);
    assert_eq!(lines[1].spans[1].text, "你e\u{301}");
    assert_eq!(lines[2].spans.len(), 1);
}

#[test]
fn styled_wrapping_retains_empty_prefix_and_overwide_grapheme_behavior() {
    let style = Style::default();
    let spans = [RenderedSpan {
        text: String::from("你a"),
        style,
    }];
    for (width, expected) in [(0, vec![]), (1, vec![">"]), (2, vec![">", " a"])] {
        let mut lines = Vec::new();
        ChatHistory::push_wrapped_spans(&mut lines, &spans, width, style, ">", " ");
        assert_eq!(
            lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>(),
            expected
        );
    }
    let mut empty = Vec::new();
    ChatHistory::push_wrapped_spans(&mut empty, &[], 3, style, ">", " ");
    assert_eq!(
        empty.iter().map(ChatHistory::line_text).collect::<Vec<_>>(),
        [">"]
    );
}

#[test]
fn wraps_wide_assistant_content_without_omitting_graphemes() {
    let assistant = message("1", ChatRole::Assistant, "你好你好");
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let rendered = history
        .build_rendered_lines(7)
        .iter()
        .map(ChatHistory::line_text)
        .collect::<Vec<_>>();

    assert_eq!(rendered, [" • 你好", "   你好"]);

    let area = Rect::new(0, 0, 7, 2);
    let mut buffer = Buffer::empty(area);
    history.render(area, &mut buffer);
    assert_eq!(buffer[(3, 0)].symbol(), "你");
    assert_eq!(buffer[(5, 0)].symbol(), "好");
    assert_eq!(buffer[(3, 1)].symbol(), "你");
    assert_eq!(buffer[(5, 1)].symbol(), "好");
}

#[test]
fn keeps_wide_markdown_graphemes_within_narrow_chat_width() {
    let assistant = message("1", ChatRole::Assistant, "> 你");
    let lines = ChatHistory::build_message_lines(&assistant, 6, &|_| ScriptCard::Waiting);

    assert!(
        lines
            .iter()
            .all(|line| display_width(&ChatHistory::line_text(line)) <= 6)
    );
}

#[test]
fn normalizes_control_characters_before_rendering_chat_history() {
    let assistant = message(
        "1",
        ChatRole::Assistant,
        "\tlet value = 1;\n\u{1b}[31mred\u{7}",
    );
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(80);
    let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

    assert!(
        rendered
            .iter()
            .any(|line| line.contains("│ let value = 1;"))
    );
    assert!(rendered.iter().any(|line| line.contains("[31mred")));
    assert!(
        rendered
            .iter()
            .all(|line| !line.chars().any(char::is_control))
    );

    let area = Rect::new(0, 0, 80, 4);
    let mut buffer = Buffer::empty(area);
    history.render(area, &mut buffer);
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| !cell.symbol().chars().any(char::is_control))
    );
}

#[test]
fn filters_system_messages_from_rendered_lines() {
    let system = message("1", ChatRole::System, "internal");
    let assistant = message("2", ChatRole::Assistant, "visible");
    let refs = [&system, &assistant];

    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(40);

    assert_eq!(lines.len(), 1);
    assert_eq!(ChatHistory::line_text(&lines[0]), " • visible");
}

#[test]
fn renders_assistant_script_call_in_pretty_format() {
    let assistant = script_message(
        "",
        "# timeout=10sec permissions=workspace-read\nopen /tmp/a.txt | lines | first 10",
        "",
    );
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(120);

    let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();
    assert_eq!(rendered.first().map(String::as_str), Some(" · not started"));
    assert!(!rendered.iter().any(|line| line.contains("timeout=")));
    assert!(
        rendered
            .iter()
            .any(|line| { line.contains("open /tmp/a.txt | lines | first 10") })
    );
    assert!(!rendered.iter().any(|line| line.contains("<tool_call>")));
}

#[test]
fn renders_mixed_assistant_text_and_script_call() {
    let assistant = script_message("before\n", "# timeout=1sec\nls", "\nafter");
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(120);
    let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

    assert_eq!(
        rendered,
        [
            " • before",
            "   ",
            " · not started",
            "   ls",
            "   ",
            "   after",
        ]
    );
}

#[test]
fn tool_delimiters_inside_scripts_and_prose_are_not_parsed() {
    let script = script_message("", "# timeout=1sec\nprint '</tool_call>'\nprint 'tail'", "");
    let lines = ChatHistory::build_message_lines(&script, 120, &|_| ScriptCard::Waiting);
    let text = lines
        .iter()
        .map(ChatHistory::line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("print '</tool_call>'"));
    assert!(text.contains("print 'tail'"));
    let prose = message(
        "1",
        ChatRole::Assistant,
        "Example: <tool_call>hello</tool_call>",
    );
    let lines = ChatHistory::build_message_lines(&prose, 120, &|_| ScriptCard::Waiting);
    assert!(
        !lines
            .iter()
            .any(|line| ChatHistory::line_text(line).contains("not started"))
    );
}

#[test]
fn renders_user_messages_with_one_cell_horizontal_padding() {
    let user = message("1", ChatRole::User, "hello");
    let refs = [&user];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(7);

    assert_eq!(lines.len(), 3);
    assert_eq!(ChatHistory::line_text(&lines[0]), "");
    assert_eq!(ChatHistory::line_text(&lines[1]), " hello");
    assert_eq!(ChatHistory::line_text(&lines[2]), "");
    let area = Rect::new(0, 0, 7, 3);
    let mut buffer = Buffer::empty(area);
    history.render(area, &mut buffer);
    let row: String = (0..7).map(|x| buffer[(x, 1)].symbol()).collect();
    assert_eq!(row, " hello ");
    let wrapped = ChatHistory::build_message_lines(&user, 6, &|_| ScriptCard::Waiting);
    assert_eq!(
        wrapped
            .iter()
            .map(ChatHistory::line_text)
            .collect::<Vec<_>>(),
        ["", " hell", " o", ""]
    );
}

#[test]
fn renders_basic_markdown_blocks_for_assistant_messages() {
    let assistant = message(
        "1",
        ChatRole::Assistant,
        "# Title\n- **one**\n1. [two](https://example.com)\n> quote\n\n`inline`",
    );
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(120);
    let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

    assert!(rendered.iter().any(|line| *line == " • Title"));
    assert!(rendered.iter().any(|line| *line == "   • one"));
    assert!(
        rendered
            .iter()
            .any(|line| *line == "   1. two (https://example.com)")
    );
    assert!(rendered.iter().any(|line| *line == "   │ quote"));
    assert!(rendered.iter().any(|line| *line == "   inline"));
}

#[test]
fn wraps_assistant_messages_with_single_first_line_indicator() {
    let assistant = message("1", ChatRole::Assistant, "abcdefghijk");
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(8);
    let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

    assert_eq!(rendered, vec![" • abcde", "   fghij", "   k"]);
}

#[test]
fn script_cards_trim_outer_blank_lines_without_losing_internal_spacing() {
    let assistant = script_message("before\n\n", "# timeout=1sec\n\nls\n\n  pwd\n\n   ", "");
    let lines = ChatHistory::build_message_lines(&assistant, 80, &|_| ScriptCard::Waiting);
    assert_eq!(
        lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>(),
        [
            " • before",
            "   ",
            " · not started",
            "   ls",
            "   ",
            "     pwd",
        ]
    );
}

#[test]
fn skips_whitespace_only_tail_after_assistant_script_call() {
    let assistant = script_message(
        "",
        "# timeout=5sec\nkraai-open-files /tmp/a.txt",
        "\n       \n\n          \n          ",
    );
    let refs = [&assistant];
    let history = ChatHistory::new(&refs, 0, true);
    let lines = history.build_rendered_lines(120);
    let rendered = lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec![" · not started", "   kraai-open-files /tmp/a.txt"]
    );
}

#[test]
fn marks_cancelled_and_failed_assistant_messages_with_a_dim_line() {
    for (status, marker) in [
        (MessageStatus::Cancelled, Some("   Cancelled")),
        (MessageStatus::Failed, Some("   Interrupted by an error")),
        (MessageStatus::Complete, None),
        (
            MessageStatus::Streaming {
                stream_id: kraai_types::StreamId::new("stream"),
            },
            None,
        ),
    ] {
        let mut assistant = message("1", ChatRole::Assistant, "partial");
        assistant.status = status;
        let lines = ChatHistory::build_message_lines(&assistant, 40, &|_| ScriptCard::Waiting);
        let texts: Vec<String> = lines.iter().map(ChatHistory::line_text).collect();
        match marker {
            Some(marker) => {
                assert_eq!(texts, [" • partial", marker]);
                let last = lines.last().unwrap();
                assert!(
                    last.spans
                        .iter()
                        .all(|span| span.style.fg == Some(Color::DarkGray))
                );
            }
            None => assert_eq!(texts, [" • partial"]),
        }
    }
}
