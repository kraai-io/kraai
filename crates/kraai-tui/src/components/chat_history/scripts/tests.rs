use super::*;

fn text(lines: &[RenderedLine]) -> Vec<String> {
    lines.iter().map(ChatHistory::line_text).collect()
}

const SOURCE: &str = "# timeout=5min permissions=workspace-write\ncargo nextest run -p kraai-tui\n";

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
    let channels = |millis: u64| {
        if let Color::Rgb(r, g, b) =
            running_dot(Duration::from_millis(millis), Some(TERMINAL_MUTED))
        {
            [r, g, b]
        } else {
            [0; 3]
        }
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
