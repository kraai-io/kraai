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

/// Where a script call that has no result yet stands.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ScriptCard {
    /// The model is still writing the call.
    Writing,
    /// The runtime waits for the user to allow the call.
    AwaitingApproval,
    /// The call is written and has not started.
    Waiting,
    Running(RunningCard),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RunningCard {
    /// How long the script has run, in whole seconds.
    pub(crate) elapsed: Duration,
    /// The colour of the dot that shows the script is alive.
    pub(crate) dot: Color,
    /// How long the script may run before the runtime stops it.
    pub(crate) timeout: Duration,
    /// The last lines it printed, oldest first.
    pub(crate) tail: Vec<ScriptOutputLine>,
}

/// How long the running dot holds each colour when the terminal's muted colour is unknown.
const DOT_BLINK: Duration = Duration::from_millis(500);

impl RunningCard {
    /// The card of a script that started `elapsed` ago, with its dot in colour `dot`.
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

/// The dot of a running script, `breath` into the breath it shares with the status line. It
/// breathes in the accent toward `muted`. Without an RGB `muted` there is nothing to fade
/// toward, so it switches between the two colours.
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

/// How many of a running script's last lines its card shows.
const TAIL_ROWS: usize = 3;
const STDERR_RAIL: &str = "   ┆ ";
const CUT: &str = "…";

/// The source as the user reads it: without the metadata header and the blank lines around it.
/// The header comes off before the text is made safe to draw, so a carriage return still ends it.
fn shown_source(source: &str) -> String {
    normalize_terminal_text(split_script_header(source).1)
        .trim_end()
        .to_owned()
}

impl ChatHistory<'_> {
    /// A script call that has no result yet, in the shape of a finished one: where it stands,
    /// its source, and the last lines it printed while it runs.
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

    /// One printed line on one row behind `rail`, ending in `CUT` when the row is too short.
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
mod tests {
    use super::*;

    fn text(lines: &[RenderedLine]) -> Vec<String> {
        lines.iter().map(ChatHistory::line_text).collect()
    }

    const SOURCE: &str =
        "# timeout=5min permissions=workspace-write\ncargo nextest run -p kraai-tui\n";

    fn line(stream: ScriptOutputStream, text: &str) -> ScriptOutputLine {
        ScriptOutputLine {
            stream,
            text: text.to_owned(),
        }
    }

    const TERMINAL_MUTED: Color = Color::Rgb(88, 82, 115);

    fn running_on(millis: u64, tail: Vec<ScriptOutputLine>, muted: Option<Color>) -> ScriptCard {
        let elapsed = Duration::from_millis(millis);
        ScriptCard::Running(RunningCard::new(
            elapsed,
            Duration::from_secs(300),
            tail,
            running_dot(elapsed, muted),
        ))
    }

    fn running(millis: u64, tail: Vec<ScriptOutputLine>) -> ScriptCard {
        running_on(millis, tail, None)
    }

    fn colour(lines: &[RenderedLine], line: usize, span: usize) -> Option<Color> {
        lines
            .get(line)
            .and_then(|line| line.spans.get(span))
            .and_then(|span| span.style.fg)
    }

    #[test]
    fn cards_without_a_result_say_where_the_script_stands_above_its_source() {
        for (card, heading, marker, words) in [
            (
                ScriptCard::Writing,
                " ◌ writing script",
                theme::MUTED,
                theme::MUTED,
            ),
            (
                ScriptCard::AwaitingApproval,
                " ● needs approval",
                theme::WAITING,
                theme::WAITING,
            ),
            (
                ScriptCard::Waiting,
                " · not started",
                theme::MUTED,
                theme::MUTED,
            ),
            (
                running(12_000, Vec::new()),
                " ● running 12s · stops at 5m",
                theme::ACCENT,
                theme::MUTED,
            ),
        ] {
            let lines = ChatHistory::build_script_card_lines(SOURCE, &card, 80);
            assert_eq!(
                text(&lines),
                [heading, "   cargo nextest run -p kraai-tui"],
                "{card:?}"
            );
            assert_eq!(colour(&lines, 0, 0), Some(marker), "{card:?}");
            assert_eq!(colour(&lines, 0, 1), Some(words), "{card:?}");
            assert_eq!(colour(&lines, 1, 0), Some(Color::White), "{card:?}");
        }
    }

    #[test]
    fn running_dot_breathes_between_the_accent_and_just_above_muted() {
        for (millis, heading, dot) in [
            (0, " ● running 0s · stops at 5m", theme::ACCENT),
            (
                300,
                " ● running 0s · stops at 5m",
                Color::Rgb(151, 157, 184),
            ),
            (
                600,
                " ● running 0s · stops at 5m",
                Color::Rgb(112, 111, 142),
            ),
            (900, " ● running 0s · stops at 5m", Color::Rgb(102, 98, 130)),
            (
                1_200,
                " ● running 1s · stops at 5m",
                Color::Rgb(112, 111, 142),
            ),
            (
                1_500,
                " ● running 1s · stops at 5m",
                Color::Rgb(131, 133, 162),
            ),
            (
                1_800,
                " ● running 1s · stops at 5m",
                Color::Rgb(151, 157, 184),
            ),
            (
                2_100,
                " ● running 2s · stops at 5m",
                Color::Rgb(167, 176, 203),
            ),
            (2_400, " ● running 2s · stops at 5m", theme::ACCENT),
            (
                65_600,
                " ● running 1m05s · stops at 5m",
                Color::Rgb(101, 97, 129),
            ),
        ] {
            let card = running_on(millis, Vec::new(), Some(TERMINAL_MUTED));
            let lines = ChatHistory::build_script_card_lines(SOURCE, &card, 80);
            assert_eq!(text(&lines).first().map(String::as_str), Some(heading));
            assert_eq!(colour(&lines, 0, 0), Some(dot), "{millis}");
            assert_eq!(colour(&lines, 0, 1), Some(theme::MUTED), "{millis}");
        }
    }

    #[test]
    fn running_dot_stays_above_muted_and_never_jumps() {
        let channels =
            |millis: u64| match running_dot(Duration::from_millis(millis), Some(TERMINAL_MUTED)) {
                Color::Rgb(r, g, b) => [r, g, b],
                _ => [0; 3],
            };
        for millis in (0..4_800).step_by(50) {
            let pairs = channels(millis).into_iter().zip(channels(millis + 50));
            for ((now, next), floor) in pairs.zip([101, 97, 129]) {
                assert!(now >= floor, "{millis}");
                assert!(now.abs_diff(next) <= 9, "{millis}: {now} to {next}");
            }
        }
    }

    #[test]
    fn running_dot_holds_one_colour_for_a_whole_frame() {
        let dot = |millis: u64| running_dot(Duration::from_millis(millis), Some(TERMINAL_MUTED));
        for millis in [0, 50, 700, 1_600, 2_350] {
            assert_eq!(dot(millis), dot(millis + 49), "{millis}");
        }
        assert_ne!(dot(700), dot(750));
    }

    #[test]
    fn running_dot_switches_twice_a_second_when_the_terminal_keeps_its_muted_colour_secret() {
        for (millis, heading, dot) in [
            (0, " ● running 0s · stops at 5m", theme::ACCENT),
            (499, " ● running 0s · stops at 5m", theme::ACCENT),
            (500, " ● running 0s · stops at 5m", theme::MUTED),
            (1_000, " ● running 1s · stops at 5m", theme::ACCENT),
            (12_500, " ● running 12s · stops at 5m", theme::MUTED),
            (65_000, " ● running 1m05s · stops at 5m", theme::ACCENT),
        ] {
            for muted in [None, Some(theme::MUTED)] {
                let card = running_on(millis, Vec::new(), muted);
                let lines = ChatHistory::build_script_card_lines(SOURCE, &card, 80);
                assert_eq!(text(&lines).first().map(String::as_str), Some(heading));
                assert_eq!(colour(&lines, 0, 0), Some(dot), "{millis}");
                assert_eq!(colour(&lines, 0, 1), Some(theme::MUTED), "{millis}");
            }
        }
        assert_eq!(
            running_dot(Duration::from_millis(500), None),
            running_dot(Duration::from_millis(999), None)
        );
    }

    #[test]
    fn running_card_shows_the_last_three_lines_one_row_each() {
        let tail = vec![
            line(ScriptOutputStream::Stdout, "one"),
            line(ScriptOutputStream::Stdout, "two"),
            line(ScriptOutputStream::Stderr, "warning: unused"),
            line(ScriptOutputStream::Stdout, "\tPASS a long test name"),
        ];
        let lines = ChatHistory::build_script_card_lines("ls", &running(1_000, tail), 24);
        assert_eq!(
            text(&lines),
            [
                " ● running 1s · stops at",
                "    5m",
                "   ls",
                "   │ two",
                "   ┆ warning: unused",
                "   │     PASS a long te…",
            ]
        );
        for row in 3..6 {
            assert_eq!(colour(&lines, row, 0), Some(theme::MUTED));
            assert_eq!(colour(&lines, row, 1), None);
        }
        for card in [
            ScriptCard::Writing,
            ScriptCard::AwaitingApproval,
            ScriptCard::Waiting,
        ] {
            assert_eq!(
                ChatHistory::build_script_card_lines("ls", &card, 24).len(),
                2
            );
        }
    }

    #[test]
    fn cards_fit_any_width_without_splitting_the_marker() {
        let tail = vec![line(ScriptOutputStream::Stderr, "warning: unused")];
        for card in [
            ScriptCard::Writing,
            ScriptCard::AwaitingApproval,
            ScriptCard::Waiting,
            running(500, tail),
        ] {
            for width in 0..12 {
                for line in ChatHistory::build_script_card_lines(SOURCE, &card, width) {
                    assert!(display_width(&ChatHistory::line_text(&line)) <= width);
                }
            }
        }
    }

    #[test]
    fn cards_drop_the_header_and_keep_other_comments() {
        for (source, shown) in [
            (
                "# timeout=5min\n\nls\n\n  pwd\n\n",
                vec!["   ls", "   ", "     pwd"],
            ),
            ("# timeout=5m", vec![]),
            ("# list files\nls", vec!["   # list files", "   ls"]),
            ("# timeout=1sec\rls", vec!["   ls"]),
            ("", vec![]),
        ] {
            let lines = ChatHistory::build_script_card_lines(source, &ScriptCard::Writing, 80);
            assert_eq!(
                text(&lines).get(1..),
                Some(
                    shown
                        .iter()
                        .map(|line| line.to_string())
                        .collect::<Vec<_>>()
                        .as_slice()
                ),
                "{source:?}"
            );
        }
    }

    #[test]
    fn cards_fit_narrow_widths() {
        let tail = vec![line(ScriptOutputStream::Stderr, "a line of output")];
        for card in [ScriptCard::Writing, running(1_500, tail)] {
            for width in 0..12 {
                for line in ChatHistory::build_script_card_lines(SOURCE, &card, width) {
                    assert!(display_width(&ChatHistory::line_text(&line)) <= width);
                }
            }
        }
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
    fn a_header_ended_by_a_carriage_return_keeps_the_script_under_it() {
        let view = ScriptView {
            mark: ScriptMark::Passed,
            summary: String::from("0.1s"),
            source: Some("# timeout=1sec\rls"),
            sections: Vec::new(),
        };
        assert_eq!(
            text(&ChatHistory::build_script_lines(&view, 24)),
            [" ✓ 0.1s", "   ls"]
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
        assert_eq!(style(2, 0), Some(Some(theme::MUTED)));
        assert_eq!(style(2, 1), Some(None));
        assert_eq!(style(5, 0), Some(Some(theme::MUTED)));
    }

    #[test]
    fn passed_and_unreadable_scripts_stay_muted_and_fit_narrow_widths() {
        for (mark, marker) in [(ScriptMark::Passed, " ✓ "), (ScriptMark::Cancelled, " ○ ")] {
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
