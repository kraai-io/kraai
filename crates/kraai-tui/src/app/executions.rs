use std::time::Duration;

use kraai_execution_types::{ScriptExecutionOutcome, ScriptExecutionStatus};
use kraai_script_protocol::{ParsedToolCallResult, ToolCallResultSection, parse_tool_call_result};

use crate::components::{ScriptMark, ScriptSection, ScriptView};
use crate::duration::format_duration;

fn mark(outcome: ScriptExecutionOutcome) -> ScriptMark {
    if outcome.succeeded() {
        ScriptMark::Passed
    } else if outcome.status == ScriptExecutionStatus::Cancelled {
        ScriptMark::Cancelled
    } else {
        ScriptMark::Failed
    }
}

/// Only the rendered result records the run time.
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
mod tests;
