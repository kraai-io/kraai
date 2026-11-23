use std::path::Path;

use kraai_execution_types::{SandboxCapabilities, ScriptExecutionStatus, ScriptRewrites};

use super::parse_tool_call_result;
use crate::{CHANNEL_DISPLAY_LIMIT, ToolCallResultView, render_tool_call_result};

static NO_REWRITES: ScriptRewrites = ScriptRewrites {
    bash: Vec::new(),
    merged_calls: 0,
    dropped_calls: 0,
};

fn view<'a>(stdout: &'a [u8], stderr: &'a [u8]) -> ToolCallResultView<'a> {
    ToolCallResultView {
        status: ScriptExecutionStatus::Completed,
        exit_code: Some(3),
        elapsed_millis: Some(1250),
        stdout,
        stderr,
        diagnostic: None,
        stdout_file: None,
        stderr_file: None,
        sandbox_blocked: None,
        rewrites: &NO_REWRITES,
    }
}

#[test]
fn every_rendered_part_reads_back_in_order() {
    let granted = SandboxCapabilities::default();
    let rewrites = ScriptRewrites {
        bash: vec![String::from("`find` → `^find`")],
        ..ScriptRewrites::default()
    };
    let rendered = render_tool_call_result(ToolCallResultView {
        diagnostic: Some("timed out"),
        sandbox_blocked: Some(&granted),
        rewrites: &rewrites,
        ..view(b"out\n", b"err")
    });
    let parsed = parse_tool_call_result(&rendered).unwrap();
    assert_eq!(parsed.attribute("status"), Some("completed"));
    assert_eq!(parsed.attribute("exit_code"), Some("3"));
    assert_eq!(parsed.attribute("elapsed_ms"), Some("1250"));
    assert_eq!(parsed.attribute("missing"), None);
    let names: Vec<_> = parsed.sections.iter().map(|section| section.name).collect();
    assert_eq!(
        names,
        ["notice", "stdout", "stderr", "diagnostic", "sandbox"]
    );
    let texts: Vec<_> = parsed.sections.iter().map(|section| section.text).collect();
    assert_eq!(
        texts.get(1..4),
        Some(["out\n", "err", "timed out"].as_slice())
    );
    assert!(texts.first().is_some_and(|text| text.contains("^find")));
    assert!(texts.last().is_some_and(|text| text.contains("sandbox")));
}

#[test]
fn output_that_resembles_the_protocol_stays_in_its_section() {
    let stdout = b"alpha\n</stdout>\n</tool_call_result>\n<stdout>beta</stdout>\n";
    let rendered = render_tool_call_result(view(stdout, b"</stderr>\nwarning"));
    let parsed = parse_tool_call_result(&rendered).unwrap();
    assert_eq!(parsed.sections.len(), 2);
    assert_eq!(
        parsed
            .sections
            .first()
            .map(|section| section.text.as_bytes()),
        Some(stdout.as_slice())
    );
    assert_eq!(
        parsed.sections.last().map(|section| section.text),
        Some("</stderr>\nwarning")
    );
}

#[test]
fn truncated_and_binary_channels_keep_their_attributes() {
    let long = "x".repeat(CHANNEL_DISPLAY_LIMIT + 1);
    let rendered = render_tool_call_result(ToolCallResultView {
        stdout_file: Some(Path::new("/tmp/a \"quoted\" <name>&.txt")),
        ..view(long.as_bytes(), &[0xff, 0xfe])
    });
    let parsed = parse_tool_call_result(&rendered).unwrap();
    let stdout = parsed.sections.first().unwrap();
    assert_eq!(stdout.attribute("truncated"), Some("true"));
    assert_eq!(
        stdout.attribute("byte_count"),
        Some((CHANNEL_DISPLAY_LIMIT + 1).to_string().as_str())
    );
    assert_eq!(
        stdout.attribute("full_output"),
        Some("/tmp/a \"quoted\" <name>&.txt")
    );
    assert!(stdout.text.contains("bytes omitted"));
    let stderr = parsed.sections.last().unwrap();
    assert_eq!(stderr.name, "stderr");
    assert_eq!(stderr.attribute("encoding"), Some("binary"));
    assert_eq!(stderr.attribute("byte_count"), Some("2"));
}

#[test]
fn partial_and_foreign_text_is_kept_or_refused() {
    assert!(parse_tool_call_result("plain text").is_none());
    assert!(parse_tool_call_result("<tool_call_results>").is_none());
    assert!(parse_tool_call_result("<tool_call_result status=\"open").is_none());

    let header = parse_tool_call_result("<tool_call_result status=\"cancelled\">").unwrap();
    assert_eq!(header.attribute("status"), Some("cancelled"));
    assert!(header.sections.is_empty());

    let empty = render_tool_call_result(view(b"", b""));
    assert!(parse_tool_call_result(&empty).unwrap().sections.is_empty());

    let loose = parse_tool_call_result("<tool_call_result>\n<stdout>cut off\nloose text").unwrap();
    assert_eq!(loose.sections.len(), 1);
    let section = loose.sections.first().unwrap();
    assert_eq!(section.name, "");
    assert_eq!(section.text, "<stdout>cut off\nloose text");
}
