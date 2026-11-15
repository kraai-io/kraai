use std::time::Duration;

use kraai_execution_types::{ScriptExecutionOutcome, ScriptExecutionStatus};
use kraai_script_protocol::{ParsedToolCallResult, ToolCallResultSection, parse_tool_call_result};

use super::duration::format_duration;
use crate::components::{ScriptMark, ScriptSection, ScriptView};

fn mark(outcome: ScriptExecutionOutcome) -> ScriptMark {
    if outcome.succeeded() {
        ScriptMark::Passed
    } else if outcome.status == ScriptExecutionStatus::Cancelled {
        ScriptMark::Cancelled
    } else {
        ScriptMark::Failed
    }
}

/// The run time, which only the rendered result records.
fn elapsed_millis(parsed: Option<&ParsedToolCallResult<'_>>) -> Option<u64> {
    parsed?.attribute("elapsed_ms")?.parse().ok()
}

fn summary(outcome: ScriptExecutionOutcome, elapsed: Option<u64>) -> String {
    let mut summary = match (outcome.status, outcome.exit_code) {
        (ScriptExecutionStatus::Cancelled, _) => String::from("cancelled"),
        (ScriptExecutionStatus::Completed, Some(code)) => format!("exit {code}"),
        (ScriptExecutionStatus::Completed, None) => String::from("no exit code"),
        (status, code) => {
            let status = status.as_str().replace('-', " ");
            match code {
                Some(code) => format!("{status} · exit {code}"),
                None => status,
            }
        }
    };
    if let Some(elapsed) = elapsed {
        summary.push_str(&format!(
            " · {}",
            format_duration(Duration::from_millis(elapsed))
        ));
    }
    summary
}

fn section_label(section: &ToolCallResultSection<'_>) -> String {
    let mut parts = Vec::new();
    if section.attribute("encoding") == Some("binary") {
        parts.push(String::from("binary"));
    }
    if let Some(bytes) = section.attribute("byte_count") {
        parts.push(format!("{bytes} bytes"));
    }
    if let Some(path) = section.attribute("full_output") {
        parts.push(format!("full output in {path}"));
    }
    if section.name.is_empty() || (section.name == "stdout" && parts.is_empty()) {
        return String::new();
    }
    parts.insert(0, section.name.to_owned());
    parts.join(" · ")
}

/// What the expanded chat shows for one script result: the sections of its `output`, then the
/// `images` it returned, one label a line.
pub(super) fn script_view<'a>(
    outcome: ScriptExecutionOutcome,
    output: &'a str,
    images: &'a str,
    source: Option<&'a str>,
) -> ScriptView<'a> {
    let parsed = parse_tool_call_result(output);
    let sections = match &parsed {
        Some(parsed) => parsed
            .sections
            .iter()
            .map(|section| ScriptSection {
                label: section_label(section),
                text: section.text,
            })
            .collect(),
        None => vec![ScriptSection {
            label: String::new(),
            text: output,
        }],
    };
    ScriptView {
        mark: mark(outcome),
        summary: summary(outcome, elapsed_millis(parsed.as_ref())),
        source,
        sections: sections
            .into_iter()
            .chain([ScriptSection {
                label: String::from("images"),
                text: images,
            }])
            .filter(|section: &ScriptSection<'_>| !section.text.trim().is_empty())
            .collect(),
    }
}

pub(super) fn group_summary(
    results: impl Iterator<Item = (ScriptExecutionOutcome, impl AsRef<str>)>,
) -> String {
    let mut count = 0;
    let mut failed = 0;
    let mut cancelled = 0;
    let mut elapsed = Some(0u64);
    for (outcome, output) in results {
        count += 1;
        match mark(outcome) {
            ScriptMark::Passed => {}
            ScriptMark::Failed => failed += 1,
            ScriptMark::Cancelled => cancelled += 1,
        }
        elapsed = elapsed
            .zip(elapsed_millis(
                parse_tool_call_result(output.as_ref()).as_ref(),
            ))
            .and_then(|(total, next)| total.checked_add(next));
    }
    let mut summary = format!("{count} {}", if count == 1 { "script" } else { "scripts" });
    for (count, label) in [(failed, "failed"), (cancelled, "cancelled")] {
        if count > 0 {
            summary.push_str(&format!(" · {count} {label}"));
        }
    }
    if let Some(elapsed) = elapsed {
        summary.push_str(&format!(
            " · {}",
            format_duration(Duration::from_millis(elapsed))
        ));
    }
    summary
}

#[cfg(test)]
mod tests {
    use kraai_execution_types::{ScriptExecutionOutcome, ScriptExecutionStatus};

    use super::{group_summary, script_view};
    use crate::components::ScriptMark;

    const fn outcome(
        status: ScriptExecutionStatus,
        exit_code: Option<i32>,
    ) -> ScriptExecutionOutcome {
        ScriptExecutionOutcome { status, exit_code }
    }

    const PASSED: ScriptExecutionOutcome = outcome(ScriptExecutionStatus::Completed, Some(0));

    #[test]
    fn groups_count_failures_and_cancellations_and_sum_durations() {
        let results = [
            (PASSED, "<tool_call_result elapsed_ms=\"100\">"),
            (
                outcome(ScriptExecutionStatus::Completed, Some(2)),
                "<tool_call_result elapsed_ms=\"200\">",
            ),
            (
                outcome(ScriptExecutionStatus::TimedOut, None),
                "<tool_call_result elapsed_ms=\"300\">",
            ),
            (
                outcome(ScriptExecutionStatus::Cancelled, None),
                "<tool_call_result elapsed_ms=\"400\">",
            ),
            (
                outcome(ScriptExecutionStatus::Completed, None),
                "<tool_call_result elapsed_ms=\"500\">",
            ),
        ];
        assert_eq!(
            group_summary(results.into_iter()),
            "5 scripts · 3 failed · 1 cancelled · 1.5s"
        );
    }

    #[test]
    fn the_structured_outcome_wins_over_what_the_text_claims() {
        let failed = outcome(ScriptExecutionStatus::RuntimeError, None);
        let text = "<tool_call_result status=\"completed\" exit_code=\"0\" elapsed_ms=\"100\">";
        assert_eq!(
            group_summary([(failed, text)].into_iter()),
            "1 script · 1 failed · 0.1s"
        );
        let view = script_view(failed, text, "", None);
        assert!(view.mark == ScriptMark::Failed);
        assert_eq!(view.summary, "runtime error · 0.1s");

        let view = script_view(
            PASSED,
            "<tool_call_result status=\"runtime-error\">",
            "",
            None,
        );
        assert!(view.mark == ScriptMark::Passed);
        assert_eq!(view.summary, "exit 0");
    }

    #[test]
    fn groups_do_not_report_partial_or_overflowed_durations() {
        let valid = "<tool_call_result elapsed_ms=\"100\">";
        for other in [
            "<tool_call_result>",
            "<tool_call_result elapsed_ms=\"invalid\">",
            "<tool_call_result elapsed_ms=\"-1\">",
            "<tool_call_result elapsed_ms=\"18446744073709551615\">",
            "not a result",
        ] {
            for results in [[valid, other], [other, valid]] {
                assert_eq!(
                    group_summary(results.into_iter().map(|output| (PASSED, output))),
                    "2 scripts"
                );
            }
        }
        assert_eq!(
            group_summary([(PASSED, "<tool_call_result elapsed_ms=\"0\">")].into_iter()),
            "1 script · 0s"
        );
    }

    #[test]
    fn summaries_show_exit_or_cancellation_without_redundant_labels() {
        for (status, exit_code, expected) in [
            (ScriptExecutionStatus::Completed, Some(1), "exit 1 · 0.5s"),
            (
                ScriptExecutionStatus::Completed,
                None,
                "no exit code · 0.5s",
            ),
            (
                ScriptExecutionStatus::Cancelled,
                Some(1),
                "cancelled · 0.5s",
            ),
            (
                ScriptExecutionStatus::TimedOut,
                Some(1),
                "timed out · exit 1 · 0.5s",
            ),
            (
                ScriptExecutionStatus::FailedToStart,
                None,
                "failed to start · 0.5s",
            ),
        ] {
            let view = script_view(
                outcome(status, exit_code),
                "<tool_call_result elapsed_ms=\"500\"></tool_call_result>",
                "",
                None,
            );
            assert_eq!(view.summary, expected);
            assert!(
                view.mark
                    == if status == ScriptExecutionStatus::Cancelled {
                        ScriptMark::Cancelled
                    } else {
                        ScriptMark::Failed
                    }
            );
        }
    }

    #[test]
    fn view_names_channels_only_when_that_adds_something() {
        let output = "<tool_call_result status=\"completed\" exit_code=\"2\" elapsed_ms=\"500\">\n\
<notice>rewrote find</notice>\n\
<stdout>plain\n</stdout>\n\
<stderr encoding=\"binary\" byte_count=\"9\" full_output=\"/tmp/err\">kept</stderr>\n\
</tool_call_result>";
        let view = script_view(
            outcome(ScriptExecutionStatus::Completed, Some(2)),
            output,
            "",
            Some("ls"),
        );
        assert!(view.mark == ScriptMark::Failed);
        assert_eq!(view.summary, "exit 2 · 0.5s");
        assert_eq!(view.source, Some("ls"));
        let sections: Vec<_> = view
            .sections
            .iter()
            .map(|section| (section.label.as_str(), section.text))
            .collect();
        assert_eq!(
            sections,
            [
                ("notice", "rewrote find"),
                ("", "plain\n"),
                (
                    "stderr · binary · 9 bytes · full output in /tmp/err",
                    "kept"
                ),
            ]
        );

        let truncated = "<tool_call_result status=\"completed\" exit_code=\"0\">\n\
<stdout byte_count=\"40000\" truncated=\"true\">head</stdout>\n\
</tool_call_result>";
        let view = script_view(PASSED, truncated, "", None);
        assert!(view.mark == ScriptMark::Passed);
        assert_eq!(
            view.sections.first().map(|section| section.label.as_str()),
            Some("stdout · 40000 bytes")
        );
    }

    #[test]
    fn view_keeps_text_it_cannot_read_and_drops_blank_output() {
        let view = script_view(PASSED, "not a result", "", None);
        assert_eq!(view.summary, "exit 0");
        assert_eq!(view.sections.len(), 1);
        assert_eq!(
            view.sections.first().map(|section| section.text),
            Some("not a result")
        );

        let cancelled = script_view(
            outcome(ScriptExecutionStatus::Cancelled, None),
            "<tool_call_result status=\"cancelled\">\n</tool_call_result>",
            "",
            None,
        );
        assert!(cancelled.mark == ScriptMark::Cancelled);
        assert_eq!(cancelled.summary, "cancelled");
        assert!(cancelled.sections.is_empty());
    }
}
