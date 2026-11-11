use std::time::Duration;

use kraai_script_protocol::{ParsedToolCallResult, ToolCallResultSection, parse_tool_call_result};

use super::duration::format_duration;
use crate::components::{ScriptMark, ScriptSection, ScriptView};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    Cancelled,
    Unknown,
}

struct Fields<'a> {
    status: &'a str,
    exit: Option<&'a str>,
    elapsed: Option<u64>,
}

impl<'a> Fields<'a> {
    fn read(parsed: Option<&'a ParsedToolCallResult<'_>>) -> Self {
        let attribute = |name| parsed.and_then(|parsed| parsed.attribute(name));
        Self {
            status: attribute("status").unwrap_or("unknown"),
            exit: attribute("exit_code"),
            elapsed: attribute("elapsed_ms").and_then(|value| value.parse().ok()),
        }
    }

    fn outcome(&self) -> Outcome {
        match (self.status, self.exit) {
            ("completed", Some("0")) => Outcome::Passed,
            ("cancelled", _) => Outcome::Cancelled,
            ("unknown", _) | ("completed", None) => Outcome::Unknown,
            _ => Outcome::Failed,
        }
    }

    fn summary(&self) -> String {
        let mut summary = if self.status == "completed" {
            format!("exit {}", self.exit.unwrap_or("unknown"))
        } else if self.status == "cancelled" {
            String::from("cancelled")
        } else {
            match self.exit {
                Some(exit) => format!("{} · exit {exit}", self.status),
                None => self.status.to_string(),
            }
        };
        if let Some(elapsed) = self.elapsed {
            summary.push_str(&format!(
                " · {}",
                format_duration(Duration::from_millis(elapsed))
            ));
        }
        summary
    }
}

#[cfg(test)]
fn result_summary(output: &str) -> String {
    Fields::read(parse_tool_call_result(output).as_ref()).summary()
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

/// What the expanded chat shows for one script result.
pub(super) fn script_view<'a>(output: &'a str, source: Option<&'a str>) -> ScriptView<'a> {
    let parsed = parse_tool_call_result(output);
    let fields = Fields::read(parsed.as_ref());
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
        mark: match fields.outcome() {
            Outcome::Passed => ScriptMark::Passed,
            Outcome::Failed => ScriptMark::Failed,
            Outcome::Cancelled | Outcome::Unknown => ScriptMark::Other,
        },
        summary: fields.summary(),
        source,
        sections: sections
            .into_iter()
            .filter(|section: &ScriptSection<'_>| !section.text.trim().is_empty())
            .collect(),
    }
}

pub(super) fn group_summary(outputs: impl Iterator<Item = impl AsRef<str>>) -> String {
    let mut count = 0;
    let mut failed = 0;
    let mut cancelled = 0;
    let mut unknown = 0;
    let mut elapsed = Some(0u64);
    for output in outputs {
        count += 1;
        let parsed = parse_tool_call_result(output.as_ref());
        let fields = Fields::read(parsed.as_ref());
        match fields.outcome() {
            Outcome::Passed => {}
            Outcome::Failed => failed += 1,
            Outcome::Cancelled => cancelled += 1,
            Outcome::Unknown => unknown += 1,
        }
        elapsed = elapsed
            .zip(fields.elapsed)
            .and_then(|(total, next)| total.checked_add(next));
    }
    let mut summary = format!("{count} {}", if count == 1 { "script" } else { "scripts" });
    for (count, label) in [
        (failed, "failed"),
        (cancelled, "cancelled"),
        (unknown, "unknown"),
    ] {
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
    use super::{group_summary, result_summary, script_view};
    use crate::components::ScriptMark;

    #[test]
    fn groups_count_each_result_category_and_sum_durations() {
        let outputs = [
            "<tool_call_result status=\"completed\" exit_code=\"0\" elapsed_ms=\"100\">",
            "<tool_call_result status=\"completed\" exit_code=\"2\" elapsed_ms=\"200\">",
            "<tool_call_result status=\"timed-out\" elapsed_ms=\"300\">",
            "<tool_call_result status=\"cancelled\" elapsed_ms=\"400\">",
            "<tool_call_result status=\"unknown\" elapsed_ms=\"500\">",
            "<tool_call_result status=\"completed\" elapsed_ms=\"600\">",
        ];
        assert_eq!(
            group_summary(outputs.into_iter()),
            "6 scripts · 2 failed · 1 cancelled · 2 unknown · 2.1s"
        );
    }

    #[test]
    fn groups_do_not_report_partial_or_overflowed_durations() {
        let valid = "<tool_call_result status=\"completed\" exit_code=\"0\" elapsed_ms=\"100\">";
        for duration in [
            "",
            " elapsed_ms=\"invalid\"",
            " elapsed_ms=\"-1\"",
            " elapsed_ms=\"18446744073709551615\"",
        ] {
            let other =
                format!("<tool_call_result status=\"completed\" exit_code=\"0\"{duration}>");
            assert_eq!(
                group_summary([valid, other.as_str()].into_iter()),
                "2 scripts"
            );
            assert_eq!(
                group_summary([other.as_str(), valid].into_iter()),
                "2 scripts"
            );
        }
    }

    #[test]
    fn groups_handle_single_unknown_and_zero_duration_results() {
        assert_eq!(
            group_summary(["not an XML result"].into_iter()),
            "1 script · 1 unknown"
        );
        assert_eq!(
            group_summary(
                ["<tool_call_result status=\"completed\" exit_code=\"0\" elapsed_ms=\"0\">"]
                    .into_iter()
            ),
            "1 script · 0s"
        );
    }

    #[test]
    fn summaries_show_exit_or_cancellation_without_redundant_labels() {
        for (status, expected) in [
            ("completed", "exit 1 · 0.5s"),
            ("cancelled", "cancelled · 0.5s"),
            ("timed-out", "timed-out · exit 1 · 0.5s"),
        ] {
            assert_eq!(
                result_summary(&format!(
                    "<tool_call_result status=\"{status}\" exit_code=\"1\" elapsed_ms=\"500\"></tool_call_result>"
                )),
                expected
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
        let view = script_view(output, Some("ls"));
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
        let view = script_view(truncated, None);
        assert!(view.mark == ScriptMark::Passed);
        assert_eq!(
            view.sections.first().map(|section| section.label.as_str()),
            Some("stdout · 40000 bytes")
        );
    }

    #[test]
    fn view_keeps_text_it_cannot_read_and_drops_blank_output() {
        let view = script_view("not a result", None);
        assert!(view.mark == ScriptMark::Other);
        assert_eq!(view.summary, "unknown");
        assert_eq!(view.sections.len(), 1);
        assert_eq!(
            view.sections.first().map(|section| section.text),
            Some("not a result")
        );

        let cancelled = script_view(
            "<tool_call_result status=\"cancelled\">\n</tool_call_result>",
            None,
        );
        assert!(cancelled.mark == ScriptMark::Other);
        assert_eq!(cancelled.summary, "cancelled");
        assert!(cancelled.sections.is_empty());
    }
}
