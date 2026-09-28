use markdown::{ParseOptions, mdast::Node};
use ratatui::style::{Color, Modifier, Style};

use super::{ChatHistory, RenderedLine, RenderedSpan};

mod table;

pub(super) const ACCENT: Color = Color::Rgb(174, 184, 210);

pub(super) fn render_message(content: &str, width: usize, normal: Style) -> Vec<RenderedLine> {
    let Ok(root) = markdown::to_mdast(content, &ParseOptions::gfm()) else {
        return ChatHistory::wrap_with_prefix(content, width, "", "")
            .into_iter()
            .map(|text| ChatHistory::single_span_line(text, normal))
            .collect();
    };
    blocks(&root, width, normal)
}

fn blocks(node: &Node, width: usize, normal: Style) -> Vec<RenderedLine> {
    let mut lines = Vec::new();
    match node {
        Node::Heading(_) | Node::Paragraph(_) => {
            let style = if matches!(node, Node::Heading(_)) {
                normal.fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                normal
            };
            let spans = inline(node, style);
            let mut part = Vec::new();
            for span in spans {
                for (index, text) in span.text.split('\n').enumerate() {
                    if index > 0 {
                        ChatHistory::push_wrapped_spans(&mut lines, &part, width, style, "", "");
                        part.clear();
                    }
                    part.push(RenderedSpan {
                        text: text.to_string(),
                        style: span.style,
                    });
                }
            }
            ChatHistory::push_wrapped_spans(&mut lines, &part, width, style, "", "");
        }
        Node::Code(code) => {
            let label = code.lang.as_deref().unwrap_or("code");
            ChatHistory::push_wrapped_lines(&mut lines, label, width, normal.fg(ACCENT), "", "");
            for source in code.value.split('\n') {
                let mut row = Vec::new();
                ChatHistory::push_wrapped_spans(
                    &mut row,
                    &[RenderedSpan {
                        text: source.to_string(),
                        style: normal,
                    }],
                    width.saturating_sub(2),
                    normal,
                    "",
                    "",
                );
                lines.extend(prefixed(row, "│ ", "│ ", normal.fg(Color::DarkGray), width));
            }
        }
        Node::Table(table) => lines = table::render(table, width, normal),
        Node::List(list) => {
            for (index, item) in list.children.iter().enumerate() {
                let mut marker = if list.ordered {
                    format!("{}. ", list.start.unwrap_or(1).saturating_add(index as u32))
                } else {
                    String::from("• ")
                };
                if let Node::ListItem(item) = item
                    && let Some(checked) = item.checked
                {
                    marker.push_str(if checked { "[x] " } else { "[ ] " });
                }
                let indent = " ".repeat(crate::components::display_width(&marker));
                let child = blocks(item, width.saturating_sub(indent.len()), normal);
                lines.extend(prefixed(child, &marker, &indent, normal, width));
            }
        }
        Node::Blockquote(_) => {
            let mut child = Vec::new();
            for node in node.children().into_iter().flatten() {
                append_block(
                    &mut child,
                    blocks(node, width.saturating_sub(2), normal),
                    normal,
                );
            }
            lines = prefixed(child, "│ ", "│ ", normal.fg(Color::DarkGray), width);
        }
        Node::ThematicBreak(_) => {
            lines.push(ChatHistory::single_span_line(
                "─".repeat(width.min(32)),
                normal.fg(Color::DarkGray),
            ));
        }
        Node::Definition(_) => {}
        _ => {
            if let Some(children) = node.children() {
                for child in children {
                    append_block(&mut lines, blocks(child, width, normal), normal);
                }
            } else {
                ChatHistory::push_wrapped_lines(
                    &mut lines,
                    &node.to_string(),
                    width,
                    normal,
                    "",
                    "",
                );
            }
        }
    }
    lines
}

pub(super) fn append_block(lines: &mut Vec<RenderedLine>, next: Vec<RenderedLine>, normal: Style) {
    if next.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(ChatHistory::single_span_line(String::new(), normal));
    }
    lines.extend(next);
}

fn prefixed(
    lines: Vec<RenderedLine>,
    first: &str,
    rest: &str,
    style: Style,
    width: usize,
) -> Vec<RenderedLine> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            let prefix = if index == 0 { first } else { rest };
            line.spans.insert(
                0,
                RenderedSpan {
                    text: crate::components::fitting_prefix(prefix, width)
                        .0
                        .to_string(),
                    style,
                },
            );
            line
        })
        .collect()
}

fn inline(node: &Node, style: Style) -> Vec<RenderedSpan> {
    let style = match node {
        Node::Strong(_) => style.add_modifier(Modifier::BOLD),
        Node::Emphasis(_) => style.add_modifier(Modifier::ITALIC),
        Node::Delete(_) => style.add_modifier(Modifier::CROSSED_OUT),
        Node::InlineCode(_) => style.fg(ACCENT),
        Node::Link(_) => style.add_modifier(Modifier::UNDERLINED),
        _ => style,
    };
    let mut spans = Vec::new();
    if let Some(children) = node.children() {
        for child in children {
            spans.extend(inline(child, style));
        }
    } else {
        let text = match node {
            Node::Break(_) => String::from("\n"),
            Node::Image(image) => image.alt.clone(),
            Node::ImageReference(image) => image.alt.clone(),
            _ => node.to_string(),
        };
        spans.push(RenderedSpan {
            text: crate::components::normalize_terminal_text(&text).into_owned(),
            style,
        });
    }
    if let Node::Link(link) = node {
        let label: String = spans.iter().map(|span| span.text.as_str()).collect();
        if label != link.url {
            spans.push(RenderedSpan {
                text: format!(
                    " ({})",
                    crate::components::normalize_terminal_text(&link.url)
                ),
                style,
            });
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
