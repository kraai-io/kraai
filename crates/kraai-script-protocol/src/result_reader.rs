const SECTIONS: [&str; 5] = ["notice", "stdout", "stderr", "diagnostic", "sandbox"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallResultSection<'a> {
    /// The tag name. Empty for text after the last section the reader recognised.
    pub name: &'a str,
    pub attributes: Vec<(&'a str, String)>,
    pub text: &'a str,
}

impl ToolCallResultSection<'_> {
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        attribute(&self.attributes, name)
    }
}

/// The text `render_tool_call_result` writes, split back into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCallResult<'a> {
    pub attributes: Vec<(&'a str, String)>,
    pub sections: Vec<ToolCallResultSection<'a>>,
}

impl ParsedToolCallResult<'_> {
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        attribute(&self.attributes, name)
    }
}

fn attribute<'a>(attributes: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.as_str())
}

/// Reads a rendered result for display. Channel text is not escaped, so output that itself
/// contains a closing tag followed by a later section's opening tag ends its section early; the
/// text is kept either way.
#[must_use]
pub fn parse_tool_call_result(rendered: &str) -> Option<ParsedToolCallResult<'_>> {
    let (attributes, body) = open_tag(rendered, "tool_call_result")?;
    let body = body.strip_suffix("</tool_call_result>").unwrap_or(body);
    let mut rest = body.strip_prefix('\n').unwrap_or(body);
    let mut sections = Vec::new();
    for (index, name) in SECTIONS.iter().enumerate() {
        let Some((attributes, content)) = open_tag(rest, name) else {
            continue;
        };
        let close = format!("</{name}>\n");
        let later = SECTIONS.get(index + 1..).unwrap_or_default();
        let split = content
            .match_indices(&close)
            .filter_map(|(at, _)| Some((content.get(..at)?, content.get(at + close.len()..)?)))
            .find(|(_, after)| {
                after.is_empty() || later.iter().any(|name| open_tag(after, name).is_some())
            });
        let Some((text, after)) = split else {
            break;
        };
        sections.push(ToolCallResultSection {
            name,
            attributes,
            text,
        });
        rest = after;
    }
    if !rest.is_empty() {
        sections.push(ToolCallResultSection {
            name: "",
            attributes: Vec::new(),
            text: rest,
        });
    }
    Some(ParsedToolCallResult {
        attributes,
        sections,
    })
}

type OpenTag<'a> = (Vec<(&'a str, String)>, &'a str);

/// The attributes of `<name ...>` at the start of `text`, and the text after the tag.
fn open_tag<'a>(text: &'a str, name: &str) -> Option<OpenTag<'a>> {
    let mut rest = text.strip_prefix('<')?.strip_prefix(name)?;
    let mut attributes = Vec::new();
    loop {
        if let Some(rest) = rest.strip_prefix('>') {
            return Some((attributes, rest));
        }
        let (key, value) = rest.strip_prefix(' ')?.split_once("=\"")?;
        let (value, after) = value.split_once('"')?;
        attributes.push((
            key,
            value
                .replace("&quot;", "\"")
                .replace("&lt;", "<")
                .replace("&amp;", "&"),
        ));
        rest = after;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use kraai_execution_types::{SandboxCapabilities, ScriptExecutionStatus};

    use super::parse_tool_call_result;
    use crate::{CHANNEL_DISPLAY_LIMIT, ToolCallResultView, render_tool_call_result};

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
            rewrites: &[],
        }
    }

    #[test]
    #[expect(clippy::unwrap_used, reason = "rendered fixtures always parse")]
    fn every_rendered_part_reads_back_in_order() {
        let granted = SandboxCapabilities::default();
        let rewrites = [String::from("`find` → `^find`")];
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
    #[expect(clippy::unwrap_used, reason = "rendered fixtures always parse")]
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
    #[expect(clippy::unwrap_used, reason = "rendered fixtures always parse")]
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
    #[expect(clippy::unwrap_used, reason = "these inputs carry a result header")]
    fn partial_and_foreign_text_is_kept_or_refused() {
        assert!(parse_tool_call_result("plain text").is_none());
        assert!(parse_tool_call_result("<tool_call_results>").is_none());
        assert!(parse_tool_call_result("<tool_call_result status=\"open").is_none());

        let header = parse_tool_call_result("<tool_call_result status=\"cancelled\">").unwrap();
        assert_eq!(header.attribute("status"), Some("cancelled"));
        assert!(header.sections.is_empty());

        let empty = render_tool_call_result(view(b"", b""));
        assert!(parse_tool_call_result(&empty).unwrap().sections.is_empty());

        let loose =
            parse_tool_call_result("<tool_call_result>\n<stdout>cut off\nloose text").unwrap();
        assert_eq!(loose.sections.len(), 1);
        let section = loose.sections.first().unwrap();
        assert_eq!(section.name, "");
        assert_eq!(section.text, "<stdout>cut off\nloose text");
    }
}
