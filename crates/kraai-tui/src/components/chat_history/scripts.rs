use ratatui::style::{Color, Style};

use super::{ChatHistory, RenderedLine, RenderedSpan};
use crate::{
    components::{display_width, normalize_terminal_text},
    theme,
};

const INDENT: &str = "   ";
const RAIL: &str = "   │ ";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptMark {
    Passed,
    Failed,
    /// Cancelled, or a result whose outcome cannot be read.
    Other,
}

pub(crate) struct ScriptSection<'a> {
    /// Names the output when it is not plain stdout; empty otherwise.
    pub(crate) label: String,
    pub(crate) text: &'a str,
}

pub(crate) struct ScriptView<'a> {
    pub(crate) mark: ScriptMark,
    pub(crate) summary: String,
    pub(crate) source: Option<&'a str>,
    pub(crate) sections: Vec<ScriptSection<'a>>,
}

impl ChatHistory<'_> {
    /// The one line that stands for a run of scripts while their output is collapsed.
    pub(crate) fn build_script_group_lines(summary: &str, width: u16) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        Self::push_wrapped_lines(
            &mut lines,
            &format!("{summary}  [ctrl+o]"),
            usize::from(width),
            Style::default().fg(theme::MUTED),
            " ▸ ",
            INDENT,
        );
        lines
    }

    /// One script: how it ended, what ran, then its output behind a rail.
    pub(crate) fn build_script_lines(view: &ScriptView<'_>, width: u16) -> Vec<RenderedLine> {
        let width = usize::from(width);
        let muted = Style::default().fg(theme::MUTED);
        let (marker, heading) = match view.mark {
            ScriptMark::Passed => (" ✓ ", muted),
            ScriptMark::Failed => (" ✗ ", Style::default().fg(theme::DANGER)),
            ScriptMark::Other => (" ○ ", muted),
        };
        let mut lines = Vec::new();
        Self::push_wrapped_lines(&mut lines, &view.summary, width, heading, marker, INDENT);
        match view.source {
            Some(source) => Self::push_wrapped_lines(
                &mut lines,
                normalize_terminal_text(source).trim_end(),
                width,
                Style::default().fg(Color::White),
                INDENT,
                INDENT,
            ),
            None => {
                Self::push_wrapped_lines(&mut lines, "source unavailable", width, muted, INDENT, "")
            }
        }
        for section in &view.sections {
            Self::push_wrapped_lines(&mut lines, &section.label, width, muted, INDENT, INDENT);
            let text = normalize_terminal_text(section.text);
            for line in Self::wrap_with_prefix(
                text.trim_end(),
                width.saturating_sub(display_width(RAIL)),
                "",
                "",
            ) {
                lines.push(RenderedLine {
                    spans: vec![
                        RenderedSpan {
                            text: RAIL.to_owned(),
                            style: muted,
                        },
                        RenderedSpan {
                            text: line,
                            style: Style::default(),
                        },
                    ],
                    bg: None,
                });
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[RenderedLine]) -> Vec<String> {
        lines.iter().map(ChatHistory::line_text).collect()
    }

    #[test]
    fn collapsed_group_is_one_muted_line_with_the_key() {
        let lines = ChatHistory::build_script_group_lines("2 scripts · 1 failed · 2.5s", 60);
        assert_eq!(text(&lines), [" ▸ 2 scripts · 1 failed · 2.5s  [ctrl+o]"]);
        assert_eq!(
            lines
                .first()
                .and_then(|line| line.spans.first())
                .map(|span| span.style.fg),
            Some(Some(theme::MUTED))
        );
    }

    #[test]
    fn script_shows_status_source_and_railed_output_with_labels() {
        let view = ScriptView {
            mark: ScriptMark::Failed,
            summary: String::from("exit 1 · 1.2s"),
            source: Some("# timeout=10sec\ncargo build\n\n"),
            sections: vec![
                ScriptSection {
                    label: String::new(),
                    text: "Compiling\n\n  indented\n",
                },
                ScriptSection {
                    label: String::from("stderr"),
                    text: "error: a long line that wraps",
                },
            ],
        };
        let lines = ChatHistory::build_script_lines(&view, 24);
        assert_eq!(
            text(&lines),
            [
                " ✗ exit 1 · 1.2s",
                "   # timeout=10sec",
                "   cargo build",
                "   │ Compiling",
                "   │ ",
                "   │   indented",
                "   stderr",
                "   │ error: a long line ",
                "   │ that wraps",
            ]
        );
        let style = |line: usize, span: usize| {
            lines
                .get(line)
                .and_then(|line| line.spans.get(span))
                .map(|span| span.style.fg)
        };
        assert_eq!(style(0, 0), Some(Some(theme::DANGER)));
        assert_eq!(style(3, 0), Some(Some(theme::MUTED)));
        assert_eq!(style(3, 1), Some(None));
        assert_eq!(style(6, 0), Some(Some(theme::MUTED)));
    }

    #[test]
    fn passed_and_unreadable_scripts_stay_muted_and_fit_narrow_widths() {
        for (mark, marker) in [(ScriptMark::Passed, " ✓ "), (ScriptMark::Other, " ○ ")] {
            let view = ScriptView {
                mark,
                summary: String::from("exit 0"),
                source: None,
                sections: Vec::new(),
            };
            let lines = ChatHistory::build_script_lines(&view, 40);
            assert_eq!(
                text(&lines),
                [
                    format!("{marker}exit 0"),
                    String::from("   source unavailable")
                ]
            );
            assert_eq!(
                lines
                    .first()
                    .and_then(|line| line.spans.first())
                    .map(|span| span.style.fg),
                Some(Some(theme::MUTED))
            );
            for width in 0..8 {
                for line in ChatHistory::build_script_lines(&view, width) {
                    assert!(display_width(&ChatHistory::line_text(&line)) <= usize::from(width));
                }
            }
        }
    }
}
