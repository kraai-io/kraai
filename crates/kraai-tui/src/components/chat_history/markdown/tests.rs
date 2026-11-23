use super::*;

#[test]
fn reference_links_render_like_inline_links() {
    let render = |source: &str| {
        render_message(source, 80, Style::default())
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| (span.text, span.style))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    for reference in ["[docs][ref]", "[ref][]", "[ref]", "[docs][ ReF ]"] {
        let label = if reference.contains("docs") {
            "docs"
        } else {
            "ref"
        };
        let source =
            format!("{reference}\n\n[ref]: https://example.com\n[REF]: https://ignored.com");
        assert_eq!(
            render(&source),
            render(&format!("[{label}](https://example.com)"))
        );
    }
    for reference in ["[docs][A  B]", "[docs][a\tb]"] {
        let source = format!("{reference}\n\n[a b]: https://example.com");
        assert_eq!(render(&source), render("[docs](https://example.com)"));
    }
}

#[test]
fn unresolved_reference_links_remain_visible() {
    let source = "[docs][missing] [other][] [shortcut]";
    let lines = render_message(source, 80, Style::default());
    let text: String = lines
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| span.text.as_str())
        .collect();
    assert_eq!(text, source);
}

#[test]
fn nested_emphasis_and_inline_code_keep_their_styles() {
    let lines = render_message(
        "**bold and *italic*** with `a_b`",
        80,
        Style::default().fg(Color::White),
    );
    let spans: Vec<_> = lines.iter().flat_map(|line| &line.spans).collect();
    assert!(spans.iter().any(|span| {
        span.text.contains("italic")
            && span
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
    }));
    assert!(
        spans
            .iter()
            .any(|span| span.text == "a_b" && span.style.fg == Some(ACCENT))
    );
}

#[test]
fn code_preserves_blank_lines_and_unfinished_fences() {
    let lines = render_message("```rust\nfirst\n\nlast", 80, Style::default());
    let text: Vec<String> = lines
        .iter()
        .map(|line| line.spans.iter().map(|span| span.text.as_str()).collect())
        .collect();
    assert_eq!(text, ["rust", "│ first", "│ ", "│ last"]);
}
