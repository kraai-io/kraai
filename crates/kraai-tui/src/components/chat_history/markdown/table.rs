use markdown::mdast::{AlignKind, Table};
use ratatui::style::{Color, Modifier, Style};

use super::{ChatHistory, RenderedLine, RenderedSpan, inline};
use crate::components::display_width;

pub(super) fn render(table: &Table, width: usize, normal: Style) -> Vec<RenderedLine> {
    let rows: Vec<Vec<Vec<RenderedSpan>>> = table
        .children
        .iter()
        .map(|row| {
            row.children()
                .into_iter()
                .flatten()
                .map(|cell| inline(cell, normal))
                .collect()
        })
        .collect();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 || width == 0 {
        return Vec::new();
    }
    let mut widths = vec![1; columns];
    for row in &rows {
        for (size, cell) in widths.iter_mut().zip(row) {
            *size = (*size)
                .max(cell.iter().map(|span| display_width(&span.text)).sum())
                .min(width);
        }
    }
    let gaps = columns.saturating_sub(1) * 3;
    if width < columns + gaps {
        let mut lines = Vec::new();
        for row in rows {
            for cell in row {
                ChatHistory::push_wrapped_spans(&mut lines, &cell, width, normal, "", "");
            }
            lines.push(ChatHistory::single_span_line(String::new(), normal));
        }
        lines.pop();
        return lines;
    }
    while widths.iter().sum::<usize>() + gaps > width {
        if let Some(size) = widths.iter_mut().max() {
            *size -= 1;
        }
    }
    let mut lines = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let cells: Vec<Vec<RenderedLine>> = widths
            .iter()
            .enumerate()
            .map(|(column, size)| {
                let mut cell_lines = Vec::new();
                let mut cell = row.get(column).cloned().unwrap_or_default();
                if row_index == 0 {
                    for span in &mut cell {
                        span.style = span.style.add_modifier(Modifier::BOLD).fg(super::ACCENT);
                    }
                }
                ChatHistory::push_wrapped_spans(&mut cell_lines, &cell, *size, normal, "", "");
                cell_lines
            })
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..height {
            let mut spans = Vec::new();
            for (column, (cell, size)) in cells.iter().zip(&widths).enumerate() {
                if column > 0 {
                    spans.push(RenderedSpan {
                        text: String::from(" │ "),
                        style: normal.fg(Color::DarkGray),
                    });
                }
                let content = cell
                    .get(line_index)
                    .map(|line| line.spans.clone())
                    .unwrap_or_default();
                let used: usize = content.iter().map(|span| display_width(&span.text)).sum();
                let padding = size.saturating_sub(used);
                let left = match table.align.get(column) {
                    Some(AlignKind::Right) => padding,
                    Some(AlignKind::Center) => padding / 2,
                    _ => 0,
                };
                spans.push(RenderedSpan {
                    text: " ".repeat(left),
                    style: normal,
                });
                spans.extend(content);
                spans.push(RenderedSpan {
                    text: " ".repeat(padding - left),
                    style: normal,
                });
            }
            lines.push(RenderedLine {
                spans,
                bg: normal.bg,
            });
        }
        if row_index == 0 {
            lines.push(ChatHistory::single_span_line(
                widths
                    .iter()
                    .map(|size| "─".repeat(*size))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
                normal.fg(Color::DarkGray),
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
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
}
