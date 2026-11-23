use super::*;
use ratatui::style::Modifier;

#[test]
fn plain_text_wraps_at_word_boundaries() {
    assert_eq!(
        wrap_words("budget is outside its bounds", 12),
        ["budget is", "outside its", "bounds"]
    );
    assert!(wrap_words("anything", 0).is_empty());
}

#[test]
fn wraps_words_across_style_boundaries() {
    let plain = Style::default();
    let bold = plain.add_modifier(Modifier::BOLD);
    let spans = [
        RenderedSpan {
            text: "one dis".into(),
            style: plain,
        },
        RenderedSpan {
            text: "tinct word".into(),
            style: bold,
        },
    ];
    let mut lines = Vec::new();
    push_prose(&mut lines, &spans, 10, plain);
    assert_eq!(
        lines.iter().map(ChatHistory::line_text).collect::<Vec<_>>(),
        ["one", "distinct", "word"]
    );
    assert_eq!(
        lines
            .get(1)
            .and_then(|line| line.spans.get(1))
            .map(|span| span.style),
        Some(bold)
    );
}

#[test]
fn long_words_and_unicode_fit_narrow_widths() {
    for text in [
        "long/path/without/spaces",
        "你好 world e\u{301} 👩‍💻",
        "word   next",
    ] {
        for width in 2..15 {
            let mut lines = Vec::new();
            push_prose(
                &mut lines,
                &[RenderedSpan {
                    text: text.into(),
                    style: Style::default(),
                }],
                width,
                Style::default(),
            );
            let rows: Vec<_> = lines.iter().map(ChatHistory::line_text).collect();
            assert!(rows.iter().all(|row| display_width(row) <= width));
            let visible: String = rows
                .concat()
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect();
            assert_eq!(
                visible,
                text.chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
            );
        }
    }
}
