const SECTIONS: [&str; 5] = ["notice", "stdout", "stderr", "diagnostic", "sandbox"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallResultSection<'a> {
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

/// Channel text is not escaped, so output containing a closing tag followed by a later
/// section's opening tag ends its section early, though the text is kept.
#[must_use]
pub fn parse_tool_call_result(rendered: &str) -> Option<ParsedToolCallResult<'_>> {
    let (attributes, body) = open_tag(rendered, "tool_call_result")?;
    let body = body.trim_suffix("</tool_call_result>");
    let mut rest = body.trim_prefix('\n');
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
mod tests;
