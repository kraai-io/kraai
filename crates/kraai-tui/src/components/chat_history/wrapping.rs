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

pub(crate) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let style = Style::default();
    let mut lines = Vec::new();
    push_prose(
        &mut lines,
        &[RenderedSpan {
            text: text.to_owned(),
            style,
        }],
        width,
        style,
    );
    lines.iter().map(ChatHistory::line_text).collect()
}

#[cfg(test)]
mod tests;
