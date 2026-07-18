use std::collections::BTreeMap;
use std::time::Duration;

use kraai_types::{SandboxCapabilities, SandboxCapability};

use crate::ProtocolError;
use crate::duration::parse_duration;

const NAME: &str = "tool_call";

pub(crate) struct ParsedStartTag {
    pub timeout: Duration,
    pub requested_capabilities: SandboxCapabilities,
}

pub(crate) fn parse_start_tag(tag: &str) -> Result<ParsedStartTag, ProtocolError> {
    let inner = tag
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .ok_or_else(|| ProtocolError::MalformedStartTag(String::from("missing delimiters")))?;
    let tail = inner
        .strip_prefix(NAME)
        .ok_or_else(|| ProtocolError::MalformedStartTag(String::from("wrong tag name")))?;
    if tail.starts_with(|character: char| !character.is_ascii_whitespace()) && !tail.is_empty() {
        return Err(ProtocolError::MalformedStartTag(String::from(
            "tag name must be followed by whitespace or '>'",
        )));
    }
    let attributes = parse_attributes(tail)?;
    let timeout = attributes
        .get("timeout")
        .ok_or(ProtocolError::MissingTimeout)
        .and_then(|value| parse_duration(value))?;
    let requested_capabilities = if let Some(value) = attributes.get("permissions") {
        parse_permissions(value).map_err(ProtocolError::InvalidPermissions)?
    } else {
        SandboxCapabilities::new([])
            .map_err(|error| ProtocolError::InvalidPermissions(error.to_string()))?
    };
    Ok(ParsedStartTag {
        timeout,
        requested_capabilities,
    })
}

fn parse_attributes(mut input: &str) -> Result<BTreeMap<String, String>, ProtocolError> {
    let mut attributes = BTreeMap::new();
    while !input.is_empty() {
        let trimmed = input.trim_start_matches(|character: char| character.is_ascii_whitespace());
        if trimmed.len() == input.len() {
            return Err(ProtocolError::MalformedStartTag(String::from(
                "attributes must be separated by whitespace",
            )));
        }
        input = trimmed;
        if input.is_empty() {
            break;
        }
        let name_len = input
            .bytes()
            .take_while(|byte| byte.is_ascii_lowercase() || *byte == b'_')
            .count();
        if name_len == 0 {
            return Err(ProtocolError::MalformedStartTag(String::from(
                "invalid attribute name",
            )));
        }
        let name = &input[..name_len];
        input = &input[name_len..];
        input = input.trim_start_matches(|character: char| character.is_ascii_whitespace());
        input = input.strip_prefix('=').ok_or_else(|| {
            ProtocolError::MalformedStartTag(format!("attribute '{name}' is missing '='"))
        })?;
        input = input.trim_start_matches(|character: char| character.is_ascii_whitespace());
        let quote = input
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'))
            .ok_or_else(|| {
                ProtocolError::MalformedStartTag(format!("attribute '{name}' must be quoted"))
            })?;
        input = &input[quote.len_utf8()..];
        let value_end = input.find(quote).ok_or_else(|| {
            ProtocolError::MalformedStartTag(format!("attribute '{name}' has no closing quote"))
        })?;
        let value = &input[..value_end];
        input = &input[value_end + quote.len_utf8()..];
        if !matches!(name, "timeout" | "permissions") {
            return Err(ProtocolError::UnknownAttribute(String::from(name)));
        }
        if attributes
            .insert(String::from(name), String::from(value))
            .is_some()
        {
            return Err(ProtocolError::DuplicateAttribute(String::from(name)));
        }
    }
    Ok(attributes)
}

fn parse_permissions(value: &str) -> Result<SandboxCapabilities, String> {
    if value.is_empty() {
        return Err(String::from("permissions list is empty"));
    }
    let mut capabilities = Vec::new();
    for raw in value.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            return Err(String::from(
                "permissions list contains an empty capability",
            ));
        }
        let capability = match name {
            "workspace-read" => SandboxCapability::WorkspaceRead,
            "host-read" => SandboxCapability::HostRead,
            "workspace-write" => SandboxCapability::WorkspaceWrite,
            "metadata-write" => SandboxCapability::MetadataWrite,
            "host-write" => SandboxCapability::HostWrite,
            "network" => SandboxCapability::Network,
            "no-sandbox" => SandboxCapability::NoSandbox,
            _ => return Err(format!("unknown capability '{name}'")),
        };
        if capabilities.contains(&capability) {
            return Err(format!("duplicate capability '{name}'"));
        }
        capabilities.push(capability);
    }
    SandboxCapabilities::new(capabilities).map_err(|error| error.to_string())
}

#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "parser tests use direct failure messages for valid fixtures"
)]
mod tests {
    use super::parse_start_tag;
    use kraai_types::SandboxCapability;
    use std::time::Duration;

    #[test]
    fn parses_attributes_in_any_order() {
        let parsed = parse_start_tag(
            r#"<tool_call permissions="workspace-write, network" timeout="10min">"#,
        )
        .unwrap_or_else(|error| panic!("unexpected parse error: {error}"));
        assert_eq!(parsed.timeout, Duration::from_secs(600));
        assert!(
            parsed
                .requested_capabilities
                .contains(SandboxCapability::WorkspaceWrite)
        );
        assert!(
            parsed
                .requested_capabilities
                .contains(SandboxCapability::Network)
        );
    }

    #[test]
    fn rejects_unknown_duplicate_and_conflicting_attributes() {
        assert!(parse_start_tag(r#"<tool_call timeout="1sec" mystery="x">"#).is_err());
        assert!(parse_start_tag(r#"<tool_call timeout="1sec" timeout="2sec">"#).is_err());
        assert!(
            parse_start_tag(r#"<tool_call timeout="1sec" permissions="no-sandbox,network">"#)
                .is_err()
        );
    }
}
