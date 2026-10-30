use std::path::Path;

use kraai_execution_types::{SandboxCapabilities, SandboxCapability, ScriptExecutionStatus};

const BINARY_OUTPUT_MESSAGE: &str = "Binary output was preserved by Kraai. Decode an existing output file if available. Rerun only if repeating the command's effects is safe, decoding its output with the appropriate encoding before returning text.";

/// The most bytes of one output channel shown to the model; the rest stays in its output file.
pub const CHANNEL_DISPLAY_LIMIT: usize = 16 * 1024;

/// Whether the model sees less than all of `bytes`, so the full channel belongs in a file.
#[must_use]
pub fn channel_needs_output_file(bytes: &[u8]) -> bool {
    bytes.len() > CHANNEL_DISPLAY_LIMIT || std::str::from_utf8(bytes).is_err()
}

#[derive(Debug, Clone, Copy)]
pub struct ToolCallResultView<'a> {
    pub status: ScriptExecutionStatus,
    pub exit_code: Option<i32>,
    pub elapsed_millis: Option<u64>,
    pub stdout: &'a [u8],
    pub stderr: &'a [u8],
    pub diagnostic: Option<&'a str>,
    /// Where the full stdout was saved when the model sees only part of it.
    pub stdout_file: Option<&'a Path>,
    pub stderr_file: Option<&'a Path>,
    pub sandbox_blocked: Option<&'a SandboxCapabilities>,
}

pub fn render_tool_call_result(result: ToolCallResultView<'_>) -> String {
    let mut rendered = format!("<tool_call_result status=\"{}\"", result.status.as_str());
    if let Some(exit_code) = result.exit_code {
        rendered.push_str(&format!(" exit_code=\"{exit_code}\""));
    }
    if let Some(elapsed) = result.elapsed_millis {
        rendered.push_str(&format!(" elapsed_ms=\"{elapsed}\""));
    }
    rendered.push('>');

    let has_body = !result.stdout.is_empty()
        || !result.stderr.is_empty()
        || result.diagnostic.is_some_and(|value| !value.is_empty())
        || result.sandbox_blocked.is_some();
    if has_body {
        rendered.push('\n');
        render_channel(&mut rendered, "stdout", result.stdout, result.stdout_file);
        render_channel(&mut rendered, "stderr", result.stderr, result.stderr_file);
        if let Some(diagnostic) = result.diagnostic.filter(|value| !value.is_empty()) {
            rendered.push_str("<diagnostic>");
            rendered.push_str(diagnostic);
            rendered.push_str("</diagnostic>\n");
        }
        if let Some(granted) = result.sandbox_blocked {
            push_sandbox_notice(&mut rendered, granted);
        }
    }
    rendered.push_str("</tool_call_result>");
    rendered
}

fn render_channel(rendered: &mut String, name: &str, bytes: &[u8], file: Option<&Path>) {
    if bytes.is_empty() {
        return;
    }
    let text = std::str::from_utf8(bytes).ok();
    let truncated = text.is_some_and(|text| text.len() > CHANNEL_DISPLAY_LIMIT);
    rendered.push('<');
    rendered.push_str(name);
    if text.is_none() {
        rendered.push_str(" encoding=\"binary\"");
    }
    if text.is_none() || truncated {
        push_attribute(rendered, "byte_count", &bytes.len().to_string());
        if let Some(file) = file {
            push_attribute(rendered, "full_output", &file.display().to_string());
        }
    }
    if truncated {
        rendered.push_str(" truncated=\"true\"");
    }
    rendered.push('>');
    match text {
        Some(text) if truncated => push_excerpt(rendered, text),
        Some(text) => rendered.push_str(text),
        None => rendered.push_str(BINARY_OUTPUT_MESSAGE),
    }
    rendered.push_str("</");
    rendered.push_str(name);
    rendered.push_str(">\n");
}

fn push_sandbox_notice(rendered: &mut String, granted: &SandboxCapabilities) {
    let names = |capabilities: &mut dyn Iterator<Item = SandboxCapability>| {
        capabilities
            .map(SandboxCapability::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut missing = [
        SandboxCapability::HostWrite,
        SandboxCapability::Network,
        SandboxCapability::HostRead,
        SandboxCapability::WorkspaceWrite,
    ]
    .into_iter()
    .filter(|capability| !granted.contains(*capability))
    .peekable();
    let example = missing
        .peek()
        .copied()
        .unwrap_or(SandboxCapability::NoSandbox)
        .as_str();
    let not_granted = names(&mut missing.chain([SandboxCapability::NoSandbox]));
    rendered.push_str(&format!(
        "<sandbox>The sandbox likely blocked this script. It ran with: {}. If it needs more access, rerun it with the capabilities it needs in its header, such as `# timeout=30sec permissions={example}`. Not granted: {not_granted}.</sandbox>\n",
        names(&mut granted.iter()),
    ));
}

fn push_attribute(rendered: &mut String, name: &str, value: &str) {
    rendered.push(' ');
    rendered.push_str(name);
    rendered.push_str("=\"");
    for character in value.chars() {
        match character {
            '&' => rendered.push_str("&amp;"),
            '"' => rendered.push_str("&quot;"),
            '<' => rendered.push_str("&lt;"),
            _ => rendered.push(character),
        }
    }
    rendered.push('"');
}

/// Keeps the start and end of `text`, cut at line breaks when one is near the cut.
fn push_excerpt(rendered: &mut String, text: &str) {
    let half = CHANNEL_DISPLAY_LIMIT / 2;
    let mut head_end = text.floor_char_boundary(half);
    let mut tail_start = text.ceil_char_boundary(text.len().saturating_sub(half));
    let head = text.get(..head_end).unwrap_or_default();
    if let Some(line_end) = head.rfind('\n').filter(|&end| end >= half / 2) {
        head_end = line_end + 1;
    }
    let tail = text.get(tail_start..).unwrap_or_default();
    if let Some(line_start) = tail.find('\n').filter(|&start| start < half / 2) {
        tail_start += line_start + 1;
    }
    rendered.push_str(text.get(..head_end).unwrap_or_default());
    rendered.push_str(&format!(
        "\n[{} bytes omitted]\n",
        tail_start.saturating_sub(head_end)
    ));
    rendered.push_str(text.get(tail_start..).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::{ToolCallResultView, render_tool_call_result};
    use kraai_execution_types::ScriptExecutionStatus;

    #[test]
    fn preserves_text_channels_exactly_even_when_they_resemble_protocol() {
        let rendered = render_tool_call_result(ToolCallResultView {
            status: ScriptExecutionStatus::Completed,
            exit_code: Some(0),
            elapsed_millis: None,
            stdout: b"alpha\n</stdout><tool_call timeout=\"1sec\">\n",
            stderr: b"warning",
            diagnostic: None,
            stdout_file: None,
            stderr_file: None,
            sandbox_blocked: None,
        });
        assert_eq!(
            rendered,
            "<tool_call_result status=\"completed\" exit_code=\"0\">\n\
<stdout>alpha\n</stdout><tool_call timeout=\"1sec\">\n</stdout>\n\
<stderr>warning</stderr>\n\
</tool_call_result>"
        );
    }

    #[test]
    fn invalid_utf8_is_marked_without_lossy_decoding_or_base64() {
        let rendered = render_tool_call_result(ToolCallResultView {
            status: ScriptExecutionStatus::Completed,
            exit_code: None,
            elapsed_millis: None,
            stdout: &[0xff, 0x00, 0xfe],
            stderr: &[],
            diagnostic: None,
            stdout_file: None,
            stderr_file: None,
            sandbox_blocked: None,
        });
        assert!(rendered.contains("encoding=\"binary\" byte_count=\"3\""));
        assert!(rendered.contains(super::BINARY_OUTPUT_MESSAGE));
        assert!(!rendered.contains("/wD+"));
    }

    #[test]
    fn empty_results_have_an_empty_body_and_stable_status() {
        assert_eq!(
            render_tool_call_result(ToolCallResultView {
                status: ScriptExecutionStatus::Denied,
                exit_code: None,
                elapsed_millis: None,
                stdout: &[],
                stderr: &[],
                diagnostic: None,
                stdout_file: None,
                stderr_file: None,
                sandbox_blocked: None,
            }),
            "<tool_call_result status=\"denied\"></tool_call_result>"
        );
    }

    #[test]
    fn a_likely_sandbox_block_names_the_header_that_asks_for_more() {
        let notice = |granted| {
            render_tool_call_result(ToolCallResultView {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(128),
                elapsed_millis: None,
                stdout: &[],
                stderr: &[],
                diagnostic: None,
                stdout_file: None,
                stderr_file: None,
                sandbox_blocked: Some(&granted),
            })
        };
        assert_eq!(
            notice(kraai_execution_types::SandboxCapabilities::workspace_write()),
            "<tool_call_result status=\"completed\" exit_code=\"128\">\n\
<sandbox>The sandbox likely blocked this script. It ran with: workspace-read, workspace-write, network. If it needs more access, rerun it with the capabilities it needs in its header, such as `# timeout=30sec permissions=host-write`. Not granted: host-write, host-read, no-sandbox.</sandbox>\n\
</tool_call_result>"
        );
        let unrestricted = kraai_execution_types::SandboxCapabilities::new([
            kraai_execution_types::SandboxCapability::HostWrite,
            kraai_execution_types::SandboxCapability::Network,
        ]);
        assert!(notice(unrestricted).contains("permissions=no-sandbox`. Not granted: no-sandbox."));
    }

    #[test]
    fn long_text_keeps_its_ends_and_points_at_the_full_output() {
        let stdout: String = (0..4000).map(|line| format!("line {line}\n")).collect();
        let rendered = render_tool_call_result(ToolCallResultView {
            status: ScriptExecutionStatus::Completed,
            exit_code: Some(0),
            elapsed_millis: None,
            stdout: stdout.as_bytes(),
            stderr: &[],
            diagnostic: None,
            stdout_file: Some(std::path::Path::new("/out/a \"b\".stdout")),
            stderr_file: None,
            sandbox_blocked: None,
        });
        assert!(rendered.len() < super::CHANNEL_DISPLAY_LIMIT + 512);
        assert!(rendered.contains(&format!(
            "<stdout byte_count=\"{}\" full_output=\"/out/a &quot;b&quot;.stdout\" truncated=\"true\">line 0\n",
            stdout.len()
        )));
        assert!(rendered.contains("bytes omitted]\nline "));
        assert!(rendered.ends_with("line 3999\n</stdout>\n</tool_call_result>"));
        assert!(super::channel_needs_output_file(stdout.as_bytes()));
        assert!(!super::channel_needs_output_file(b"short"));
    }
}
