use std::time::Duration;

use kraai_execution_types::ScriptOutputStream;
use kraai_runtime::ScriptOutputLine;
use kraai_script_protocol::split_script_header;
use ratatui::style::{Color, Style};

use super::{ChatHistory, RenderedLine, RenderedSpan};
use crate::{
    components::{display_width, fitting_prefix, normalize_terminal_text},
    duration::{format_duration, format_limit},
    theme,
};

const INDENT: &str = "   ";
const RAIL: &str = "   │ ";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptMark {
    Passed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ScriptCard {
    Writing,
    AwaitingApproval,
    Waiting,
    Running(RunningCard),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RunningCard {
    pub(crate) elapsed: Duration,
    pub(crate) dot: Color,
    pub(crate) timeout: Duration,
    pub(crate) tail: Vec<ScriptOutputLine>,
}

const DOT_BLINK: Duration = Duration::from_millis(500);

impl RunningCard {
    /// Whole seconds are the precision the card shows.
    pub(crate) fn new(
        elapsed: Duration,
        timeout: Duration,
        tail: Vec<ScriptOutputLine>,
        dot: Color,
    ) -> Self {
        Self {
            elapsed: Duration::from_secs(elapsed.as_secs()),
            dot,
            timeout,
            tail,
        }
    }
}

/// Without an RGB `muted` there is nothing to fade toward, so it switches between the two colours.
pub(crate) fn running_dot(breath: Duration, muted: Option<Color>) -> Color {
    let fallback = if (breath.as_millis() / DOT_BLINK.as_millis()).is_multiple_of(2) {
        theme::ACCENT
    } else {
        theme::MUTED
    };
    muted
        .and_then(|muted| theme::breathe(breath, theme::ACCENT, muted))
        .unwrap_or(fallback)
}

pub(crate) struct ScriptSection<'a> {
    pub(crate) label: String,
    pub(crate) text: &'a str,
}

pub(crate) struct ScriptView<'a> {
    pub(crate) mark: ScriptMark,
    pub(crate) summary: String,
    pub(crate) source: Option<&'a str>,
    pub(crate) sections: Vec<ScriptSection<'a>>,
}

const TAIL_ROWS: usize = 3;
const STDERR_RAIL: &str = "   ┆ ";
const CUT: &str = "…";

/// The header comes off before the text is made safe to draw, so a carriage return still ends it.
fn shown_source(source: &str) -> String {
    normalize_terminal_text(split_script_header(source).1)
        .trim_end()
        .to_owned()
}

impl ChatHistory<'_> {
    pub(crate) fn build_script_card_lines(
        source: &str,
        card: &ScriptCard,
        width: usize,
    ) -> Vec<RenderedLine> {
        let muted = Style::default().fg(theme::MUTED);
        let waiting = Style::default().fg(theme::WAITING);
        let (marker, marker_style, heading, heading_style) = match card {
            ScriptCard::Writing => (" ◌ ", muted, String::from("writing script"), muted),
            ScriptCard::AwaitingApproval => {
                (" ● ", waiting, String::from("needs approval"), waiting)
            }
            ScriptCard::Waiting => (" · ", muted, String::from("not started"), muted),
            ScriptCard::Running(running) => (
                " ● ",
                Style::default().fg(running.dot),
                format!(
                    "running {} · stops at {}",
                    format_duration(running.elapsed),
                    format_limit(running.timeout)
                ),
                muted,
            ),
        };
        let mut lines = Vec::new();
        for (index, line) in Self::wrap_with_prefix(&heading, width, marker, INDENT)
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                lines.push(Self::single_span_line(line, heading_style));
                continue;
            }
            let (mark, text) = match line.strip_prefix(marker) {
                Some(text) => (marker, text),
                None => (line.as_str(), ""),
            };
            lines.push(RenderedLine {
                spans: vec![
                    RenderedSpan {
                        text: mark.to_owned(),
                        style: marker_style,
                    },
                    RenderedSpan {
                        text: text.to_owned(),
                        style: heading_style,
                    },
                ],
                bg: None,
            });
        }
        Self::push_wrapped_lines(
            &mut lines,
            &shown_source(source),
            width,
            Style::default().fg(Color::White),
            INDENT,
            INDENT,
        );
        if let ScriptCard::Running(running) = card {
            let first = running.tail.len().saturating_sub(TAIL_ROWS);
            for line in running.tail.iter().skip(first) {
                let rail = match line.stream {
                    ScriptOutputStream::Stdout => RAIL,
                    ScriptOutputStream::Stderr => STDERR_RAIL,
                };
                lines.push(Self::tail_line(rail, &line.text, width, muted));
            }
        }
        lines
    }

    fn tail_line(rail: &str, text: &str, width: usize, rail_style: Style) -> RenderedLine {
        let (rail, used) = fitting_prefix(rail, width);
        let room = width.saturating_sub(used);
        let text = normalize_terminal_text(text);
        let text = text.lines().next().unwrap_or_default();
        let shown = if display_width(text) <= room {
            text.to_owned()
        } else if room == 0 {
            String::new()
        } else {
            let (head, _) = fitting_prefix(text, room.saturating_sub(display_width(CUT)));
            format!("{head}{CUT}")
        };
        RenderedLine {
            spans: vec![
                RenderedSpan {
                    text: rail.to_owned(),
                    style: rail_style,
                },
                RenderedSpan {
                    text: shown,
                    style: Style::default(),
                },
            ],
            bg: None,
        }
    }

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

    pub(crate) fn build_script_lines(view: &ScriptView<'_>, width: u16) -> Vec<RenderedLine> {
        let width = usize::from(width);
        let muted = Style::default().fg(theme::MUTED);
        let (marker, heading) = match view.mark {
            ScriptMark::Passed => (" ✓ ", muted),
            ScriptMark::Failed => (" ✗ ", Style::default().fg(theme::DANGER)),
            ScriptMark::Cancelled => (" ○ ", muted),
        };
        let mut lines = Vec::new();
        Self::push_wrapped_lines(&mut lines, &view.summary, width, heading, marker, INDENT);
        match view.source {
            Some(source) => Self::push_wrapped_lines(
                &mut lines,
                &shown_source(source),
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
mod tests;
