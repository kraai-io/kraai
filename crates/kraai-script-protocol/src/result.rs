use kraai_types::ScriptExecutionStatus;

const BINARY_OUTPUT_MESSAGE: &str = "Binary output was preserved by Kraai. Rerun the command with an explicit text encoding to inspect it.";

#[derive(Debug, Clone, Copy)]
pub struct ToolCallResultView<'a> {
    pub status: ScriptExecutionStatus,
    pub exit_code: Option<i32>,
    pub stdout: &'a [u8],
    pub stderr: &'a [u8],
    pub diagnostic: Option<&'a str>,
}

pub fn render_tool_call_result(result: ToolCallResultView<'_>) -> String {
    let mut rendered = format!("<tool_call_result status=\"{}\"", result.status.as_str());
    if let Some(exit_code) = result.exit_code {
        rendered.push_str(&format!(" exit_code=\"{exit_code}\""));
    }
    rendered.push('>');

    let has_body = !result.stdout.is_empty()
        || !result.stderr.is_empty()
        || result.diagnostic.is_some_and(|value| !value.is_empty());
    if has_body {
        rendered.push('\n');
        render_channel(&mut rendered, "stdout", result.stdout);
        render_channel(&mut rendered, "stderr", result.stderr);
        if let Some(diagnostic) = result.diagnostic.filter(|value| !value.is_empty()) {
            rendered.push_str("<diagnostic>");
            rendered.push_str(diagnostic);
            rendered.push_str("</diagnostic>\n");
        }
    }
    rendered.push_str("</tool_call_result>");
    rendered
}

fn render_channel(rendered: &mut String, name: &str, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            rendered.push('<');
            rendered.push_str(name);
            rendered.push('>');
            rendered.push_str(text);
            rendered.push_str("</");
            rendered.push_str(name);
            rendered.push_str(">\n");
        }
        Err(_) => {
            rendered.push('<');
            rendered.push_str(name);
            rendered.push_str(" encoding=\"binary\" byte_count=\"");
            rendered.push_str(&bytes.len().to_string());
            rendered.push_str("\">");
            rendered.push_str(BINARY_OUTPUT_MESSAGE);
            rendered.push_str("</");
            rendered.push_str(name);
            rendered.push_str(">\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolCallResultView, render_tool_call_result};
    use kraai_types::ScriptExecutionStatus;

    #[test]
    fn preserves_text_channels_exactly_even_when_they_resemble_protocol() {
        let rendered = render_tool_call_result(ToolCallResultView {
            status: ScriptExecutionStatus::Completed,
            exit_code: Some(0),
            stdout: b"alpha\n</stdout><tool_call timeout=\"1sec\">\n",
            stderr: b"warning",
            diagnostic: None,
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
            stdout: &[0xff, 0x00, 0xfe],
            stderr: &[],
            diagnostic: None,
        });
        assert!(rendered.contains("encoding=\"binary\" byte_count=\"3\""));
        assert!(rendered.contains("Rerun the command with an explicit text encoding"));
        assert!(!rendered.contains("/wD+"));
    }

    #[test]
    fn empty_results_have_an_empty_body_and_stable_status() {
        assert_eq!(
            render_tool_call_result(ToolCallResultView {
                status: ScriptExecutionStatus::Denied,
                exit_code: None,
                stdout: &[],
                stderr: &[],
                diagnostic: None,
            }),
            "<tool_call_result status=\"denied\"></tool_call_result>"
        );
    }
}
