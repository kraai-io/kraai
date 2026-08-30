use std::collections::BTreeMap;

use kraai_types::{SandboxCapabilities, SandboxCapability};

use crate::duration::parse_duration;
use crate::{ProtocolError, ScriptBlock};

pub const SCRIPT_METADATA_PREFIX: &str = "# kraai";

pub fn parse_script_input(input: &str) -> Result<ScriptBlock, ProtocolError> {
    let header_start = input
        .char_indices()
        .find_map(|(index, character)| (!character.is_ascii_whitespace()).then_some(index))
        .ok_or(ProtocolError::EmptyScript)?;
    let header_tail = &input[header_start..];
    let header_end = header_tail.find(['\n', '\r']).unwrap_or(header_tail.len());
    let header = &header_tail[..header_end];
    let fields = header
        .strip_prefix(SCRIPT_METADATA_PREFIX)
        .filter(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
        .ok_or_else(|| {
            ProtocolError::MalformedMetadata(format!(
                "first non-empty line must start with '{SCRIPT_METADATA_PREFIX}'"
            ))
        })?;
    let attributes = parse_fields(fields)?;
    let timeout = attributes
        .get("timeout")
        .ok_or(ProtocolError::MissingTimeout)
        .and_then(|value| parse_duration(value))?;
    let requested_capabilities = attributes
        .get("permissions")
        .map(|value| parse_permissions(value).map_err(ProtocolError::InvalidPermissions))
        .transpose()?
        .unwrap_or_default();

    let source_start = header_start + header_end;
    let source = input[source_start..]
        .trim_start_matches(['\r', '\n'])
        .as_bytes()
        .to_vec();
    if source.iter().all(u8::is_ascii_whitespace) {
        return Err(ProtocolError::EmptyScript);
    }

    Ok(ScriptBlock {
        input: input.to_string(),
        source,
        timeout,
        requested_capabilities,
    })
}

fn parse_fields(input: &str) -> Result<BTreeMap<String, String>, ProtocolError> {
    let mut attributes = BTreeMap::new();
    for field in input.split_ascii_whitespace() {
        let (name, value) = field.split_once('=').ok_or_else(|| {
            ProtocolError::MalformedMetadata(format!("field '{field}' is missing '='"))
        })?;
        if !matches!(name, "timeout" | "permissions") {
            return Err(ProtocolError::UnknownAttribute(name.to_string()));
        }
        if value.is_empty() {
            return Err(ProtocolError::MalformedMetadata(format!(
                "field '{name}' is empty"
            )));
        }
        if attributes
            .insert(name.to_string(), value.to_string())
            .is_some()
        {
            return Err(ProtocolError::DuplicateAttribute(name.to_string()));
        }
    }
    Ok(attributes)
}

fn parse_permissions(value: &str) -> Result<SandboxCapabilities, String> {
    if value.is_empty() {
        return Err(String::from("permissions list is empty"));
    }
    let mut capabilities = Vec::new();
    for name in value.split(',') {
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
    clippy::expect_used,
    reason = "test parses a known-valid script fixture"
)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_universal_nushell_metadata_header() {
        let script = parse_script_input(
            "# kraai timeout=1.5sec permissions=workspace-write,network\nls | get name",
        )
        .expect("valid script input");

        assert_eq!(script.timeout, Duration::from_millis(1500));
        assert_eq!(script.source, b"ls | get name");
        assert!(
            script
                .requested_capabilities
                .contains(SandboxCapability::WorkspaceWrite)
        );
        assert!(
            script
                .requested_capabilities
                .contains(SandboxCapability::Network)
        );
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_metadata() {
        assert!(parse_script_input("# kraai\necho hi").is_err());
        assert!(parse_script_input("# kraai timeout=1sec timeout=2sec\necho hi").is_err());
        assert!(parse_script_input("# kraai timeout=1sec mystery=x\necho hi").is_err());
    }
}
