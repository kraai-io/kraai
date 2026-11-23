use super::*;

#[test]
fn tables_align_and_wrap_without_losing_cell_text() {
    let source =
        "| Case | Expected |\n|---|---|\n| Empty input | Error |\n| Valid token | Parsed |";
    let lines = super::super::render_message(source, 80, Style::default());
    let text: Vec<String> = lines
        .iter()
        .map(|line| line.spans.iter().map(|span| span.text.as_str()).collect())
        .collect();
    assert_eq!(
        text,
        [
            "Case        │ Expected",
            "────────────┼─────────",
            "Empty input │ Error   ",
            "Valid token │ Parsed  "
        ]
    );
    for width in 1..20 {
        let lines = super::super::render_message(source, width, Style::default());
        assert!(lines.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| display_width(&span.text))
                .sum::<usize>()
                <= width
        }));
        let mut text: Vec<char> = lines
            .iter()
            .flat_map(|line| &line.spans)
            .flat_map(|span| span.text.chars())
            .filter(|ch| ch.is_alphanumeric())
            .collect();
        let mut expected: Vec<char> = "CaseExpectedEmptyinputErrorValidtokenParsed"
            .chars()
            .collect();
        text.sort_unstable();
        expected.sort_unstable();
        assert_eq!(text, expected);
    }
}
