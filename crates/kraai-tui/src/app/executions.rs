use super::*;

fn result_fields(output: &str) -> (&str, Option<&str>, Option<u64>) {
    let header = output
        .strip_prefix("<tool_call_result ")
        .and_then(|text| text.split_once('>').map(|(header, _)| header));
    let attribute = |name: &str| {
        header.and_then(|header| {
            header.split_whitespace().find_map(|part| {
                part.strip_prefix(name)
                    .and_then(|value| value.strip_prefix("=\""))
                    .and_then(|value| value.strip_suffix('"'))
            })
        })
    };
    let status = attribute("status").unwrap_or("unknown");
    let exit = attribute("exit_code");
    let elapsed = attribute("elapsed_ms").and_then(|value| value.parse::<u64>().ok());
    (status, exit, elapsed)
}

pub(super) fn result_summary(output: &str) -> String {
    let (status, exit, elapsed) = result_fields(output);
    let mut summary = if status == "completed" {
        format!("exit {}", exit.unwrap_or("unknown"))
    } else if status == "cancelled" {
        String::from("cancelled")
    } else {
        match exit {
            Some(exit) => format!("{status} · exit {exit}"),
            None => status.to_string(),
        }
    };
    if let Some(elapsed) = elapsed {
        summary.push_str(&format!(
            " · {}",
            super::duration::format_duration(Duration::from_millis(elapsed))
        ));
    }
    summary
}

pub(super) fn group_summary(outputs: impl Iterator<Item = impl AsRef<str>>) -> String {
    let mut count = 0;
    let mut failed = 0;
    let mut cancelled = 0;
    let mut unknown = 0;
    let mut elapsed = Some(0u64);
    for output in outputs {
        count += 1;
        let (status, exit, duration) = result_fields(output.as_ref());
        match (status, exit) {
            ("completed", Some("0")) => {}
            ("cancelled", _) => cancelled += 1,
            ("unknown", _) | ("completed", None) => unknown += 1,
            _ => failed += 1,
        }
        elapsed = elapsed
            .zip(duration)
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
            super::duration::format_duration(Duration::from_millis(elapsed))
        ));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::{group_summary, result_summary};

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
}
