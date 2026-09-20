#![expect(
    clippy::expect_used,
    reason = "module-level regex constants are statically validated during development"
)]

use ratatui::style::{Color, Modifier, Style};
use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

use super::{ChatHistory, RenderedLine, RenderedSpan};

static IMAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").expect("valid regex"));
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex"));
static STRONG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*|__([^_]+)__").expect("valid regex"));
static EMPHASIS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*([^*]+)\*|_([^_]+)_").expect("valid regex"));
static STRIKE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~~([^~]+)~~").expect("valid regex"));
static ESCAPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\([\\`*_{}\[\]()#+.!~-])").expect("valid regex"));
static INLINE_CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`([^`]+)`").expect("valid regex"));
static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(#{1,6})\s+(.*)$").expect("valid regex"));
static QUOTE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>\s?(.*)$").expect("valid regex"));
static UNORDERED_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+(.*)$").expect("valid regex"));
static ORDERED_LIST_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+)\.\s+(.*)$").expect("valid regex"));
static FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*```([A-Za-z0-9_-]+)?\s*$").expect("valid regex"));

pub(super) fn render_message(
    content: &str,
    width: usize,
    normal_style: Style,
) -> Vec<RenderedLine> {
    let mut lines = Vec::new();
    let heading_style = Style::default()
        .fg(Color::Rgb(255, 220, 120))
        .add_modifier(Modifier::BOLD);
    let quote_style = Style::default()
        .fg(Color::Rgb(170, 170, 170))
        .add_modifier(Modifier::ITALIC);
    let code_style = Style::default().fg(Color::Rgb(130, 230, 255));
    let inline_code_style = Style::default().fg(Color::Rgb(255, 180, 90));
    let list_style = normal_style;

    let mut in_fenced_code = false;
    let mut code_lang = String::new();

    for source_line in content.lines() {
        if let Some(caps) = FENCE_RE.captures(source_line) {
            if in_fenced_code {
                in_fenced_code = false;
                code_lang.clear();
            } else {
                in_fenced_code = true;
                code_lang = caps
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                if !code_lang.is_empty() {
                    ChatHistory::push_wrapped_lines(
                        &mut lines,
                        &format!("[code: {code_lang}]"),
                        width,
                        code_style,
                        "",
                        "",
                    );
                }
            }
            continue;
        }

        if in_fenced_code {
            ChatHistory::push_wrapped_lines(&mut lines, source_line, width, code_style, "  ", "  ");
            continue;
        }

        if let Some(caps) = HEADING_RE.captures(source_line) {
            if let Some(text) = caps.get(2).map(|m| m.as_str()) {
                let spans = inline_markdown_spans(text, heading_style, inline_code_style);
                ChatHistory::push_wrapped_spans(&mut lines, &spans, width, heading_style, "", "");
            }
            continue;
        }

        if let Some(caps) = QUOTE_RE.captures(source_line)
            && let Some(text) = caps.get(1).map(|m| m.as_str())
        {
            let spans = inline_markdown_spans(text, quote_style, inline_code_style);
            ChatHistory::push_wrapped_spans(&mut lines, &spans, width, quote_style, "│ ", "│ ");
            continue;
        }

        if let Some(caps) = UNORDERED_LIST_RE.captures(source_line)
            && let Some(text) = caps.get(1).map(|m| m.as_str())
        {
            let spans = inline_markdown_spans(text, list_style, inline_code_style);
            ChatHistory::push_wrapped_spans(&mut lines, &spans, width, list_style, "• ", "  ");
            continue;
        }

        if let Some(caps) = ORDERED_LIST_RE.captures(source_line) {
            let idx = caps.get(1).map(|m| m.as_str()).unwrap_or("1");
            if let Some(text) = caps.get(2).map(|m| m.as_str()) {
                let spans = inline_markdown_spans(text, list_style, inline_code_style);
                let prefix = format!("{idx}. ");
                ChatHistory::push_wrapped_spans(
                    &mut lines, &spans, width, list_style, &prefix, "   ",
                );
                continue;
            }
        }

        let spans = inline_markdown_spans(source_line, normal_style, inline_code_style);
        ChatHistory::push_wrapped_spans(&mut lines, &spans, width, normal_style, "", "");
    }

    if lines.is_empty() {
        lines.push(ChatHistory::single_span_line(String::new(), normal_style));
    }

    lines
}

fn strip_non_code_inline_markdown(text: &str) -> String {
    let mut text = Cow::Borrowed(text);
    for (regex, replacement) in [
        (&IMAGE_RE, "$1"),
        (&LINK_RE, "$1 ($2)"),
        (&STRONG_RE, "$1$2"),
        (&EMPHASIS_RE, "$1$2"),
        (&STRIKE_RE, "$1"),
        (&ESCAPE_RE, "$1"),
    ] {
        if let Cow::Owned(replaced) = regex.replace_all(&text, replacement) {
            text = Cow::Owned(replaced);
        }
    }
    text.into_owned()
}

fn inline_markdown_spans(
    text: &str,
    base_style: Style,
    inline_code_style: Style,
) -> Vec<RenderedSpan> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;

    for caps in INLINE_CODE_RE.captures_iter(text) {
        let Some(full) = caps.get(0) else {
            continue;
        };
        let before = &text[cursor..full.start()];
        let before_plain = strip_non_code_inline_markdown(before);
        if !before_plain.is_empty() {
            spans.push(RenderedSpan {
                text: before_plain,
                style: base_style,
            });
        }

        if let Some(code) = caps.get(1).map(|m| m.as_str())
            && !code.is_empty()
        {
            spans.push(RenderedSpan {
                text: code.to_string(),
                style: inline_code_style,
            });
        }

        cursor = full.end();
    }

    let tail = &text[cursor..];
    let tail_plain = strip_non_code_inline_markdown(tail);
    if !tail_plain.is_empty() {
        spans.push(RenderedSpan {
            text: tail_plain,
            style: base_style,
        });
    }

    if spans.is_empty() {
        spans.push(RenderedSpan {
            text: String::new(),
            style: base_style,
        });
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::strip_non_code_inline_markdown;

    #[test]
    fn inline_replacements_preserve_order_and_plain_text() {
        for (input, expected) in [
            ("plain 你好 text", "plain 你好 text"),
            ("**unterminated [label", "**unterminated [label"),
            (
                "![**image**](asset) [~~link~~](url) __bold__ _italic_ \\[literal\\]",
                "image link (url) bold italic [literal]",
            ),
            ("[label](url) after", "label (url) after"),
        ] {
            assert_eq!(strip_non_code_inline_markdown(input), expected);
        }
    }
}
