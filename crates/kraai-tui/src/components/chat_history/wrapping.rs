use ratatui::style::Style;
use unicode_segmentation::UnicodeSegmentation;

use super::{ChatHistory, RenderedLine, RenderedSpan, display_width};

pub(super) fn push_prose(
    lines: &mut Vec<RenderedLine>,
    spans: &[RenderedSpan],
    width: usize,
    style: Style,
) {
    if width == 0 {
        return;
    }
    let graphemes: Vec<_> = spans
        .iter()
        .flat_map(|span| {
            span.text
                .graphemes(true)
                .map(move |text| (text, span.style, display_width(text)))
        })
        .collect();
    if graphemes.is_empty() {
        lines.push(ChatHistory::single_span_line(String::new(), style));
        return;
    }
    let mut remaining = graphemes.as_slice();
    while !remaining.is_empty() {
        let mut used = 0;
        let mut end = 0;
        let mut boundary = None;
        let mut in_word = false;
        for (index, &(text, _, size)) in remaining.iter().enumerate() {
            let whitespace = text.chars().all(char::is_whitespace);
            if whitespace && in_word {
                boundary = Some(index);
            }
            if used + size > width {
                break;
            }
            used += size;
            end = index + 1;
            in_word = !whitespace;
        }
        let wrapped = end < remaining.len();
        if wrapped && let Some(boundary) = boundary {
            end = boundary;
        }
        let mut row: Vec<RenderedSpan> = Vec::new();
        for &(text, style, _) in remaining.iter().take(end) {
            if let Some(last) = row.last_mut()
                && last.style == style
            {
                last.text.push_str(text);
            } else {
                row.push(RenderedSpan {
                    text: text.to_owned(),
                    style,
                });
            }
        }
        lines.push(RenderedLine {
            spans: row,
            bg: style.bg,
        });
        remaining = remaining.get(end.max(1)..).unwrap_or_default();
        if wrapped {
            while remaining
                .first()
                .is_some_and(|(text, _, _)| text.chars().all(char::is_whitespace))
            {
                remaining = remaining.get(1..).unwrap_or_default();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

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
}
