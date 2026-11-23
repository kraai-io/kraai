use kraai_execution_types::{SandboxCapabilities, SandboxCapability};

use crate::ProtocolError;
use crate::duration::{format_duration, parse_duration};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptBlock {
    pub input: String,
    pub source: Vec<u8>,
    pub timeout: Duration,
    pub requested_capabilities: SandboxCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidScriptBlock {
    pub input: String,
    pub source: Vec<u8>,
    pub timeout: Option<Duration>,
    pub requested_capabilities: SandboxCapabilities,
}

const SCRIPT_METADATA_PREFIX: &str = "#";

pub const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SCRIPT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

pub fn parse_script_input(input: &str) -> Result<ScriptBlock, ProtocolError> {
    let (fields, source) = split_script_header(input);
    if fields.is_none() && source.is_empty() {
        return Err(ProtocolError::EmptyScript);
    }
    let attributes = fields.map(parse_fields).transpose()?.unwrap_or_default();
    let timeout = attributes
        .timeout
        .map_or(Ok(DEFAULT_SCRIPT_TIMEOUT), parse_duration)?;
    if timeout > MAX_SCRIPT_TIMEOUT {
        return Err(ProtocolError::InvalidTimeout(String::from(
            "duration must be at most 1day",
        )));
    }
    let requested_capabilities = attributes
        .permissions
        .map(|value| parse_permissions(value).map_err(ProtocolError::InvalidPermissions))
        .transpose()?
        .unwrap_or_default();

    if source.bytes().all(|byte| byte.is_ascii_whitespace()) {
        return Err(ProtocolError::EmptyScript);
    }

    Ok(ScriptBlock {
        input: input.to_string(),
        source: source.as_bytes().to_vec(),
        timeout,
        requested_capabilities,
    })
}

pub fn merge_scripts(scripts: &[ScriptBlock]) -> String {
    let timeout = scripts
        .iter()
        .map(|script| script.timeout)
        .sum::<Duration>()
        .min(MAX_SCRIPT_TIMEOUT);
    let capabilities = SandboxCapabilities::new(
        scripts
            .iter()
            .flat_map(|script| script.requested_capabilities.iter()),
    );
    let mut fields = Vec::new();
    if timeout != DEFAULT_SCRIPT_TIMEOUT {
        fields.push(format!("timeout={}", format_duration(timeout)));
    }
    let permissions = capabilities
        .without_implied()
        .map(SandboxCapability::as_str)
        .collect::<Vec<_>>();
    if !permissions.is_empty() {
        fields.push(format!("permissions={}", permissions.join(",")));
    }
    let mut merged = String::new();
    if !fields.is_empty() {
        merged.push_str(&format!("# {}\n", fields.join(" ")));
    }
    let sources = scripts
        .iter()
        .map(|script| {
            format!(
                "do {{\n{}\n}}",
                String::from_utf8_lossy(&script.source).trim_end()
            )
        })
        .collect::<Vec<_>>();
    merged.push_str(&sources.join("\n\n"));
    merged
}

pub fn split_script_header(input: &str) -> (Option<&str>, &str) {
    let tail = input.trim_start_matches(|character: char| character.is_ascii_whitespace());
    let header_end = tail.find(['\n', '\r']).unwrap_or(tail.len());
    match metadata_fields(&tail[..header_end]) {
        Some(fields) => (
            Some(fields),
            tail[header_end..].trim_start_matches(['\r', '\n']),
        ),
        None => (None, tail),
    }
}

fn metadata_fields(line: &str) -> Option<&str> {
    line.strip_prefix(SCRIPT_METADATA_PREFIX)
        .filter(|tail| tail.starts_with(char::is_whitespace))
        .filter(|tail| {
            tail.split_ascii_whitespace()
                .any(|field| field.contains('='))
        })
}

#[derive(Default)]
struct ScriptMetadata<'a> {
    timeout: Option<&'a str>,
    permissions: Option<&'a str>,
}

fn parse_fields(input: &str) -> Result<ScriptMetadata<'_>, ProtocolError> {
    let mut attributes = ScriptMetadata::default();
    for field in input.split_ascii_whitespace() {
        let (name, value) = field.split_once('=').ok_or_else(|| {
            ProtocolError::MalformedMetadata(format!("field '{field}' is missing '='"))
        })?;
        let attribute = match name {
            "timeout" => &mut attributes.timeout,
            "permissions" => &mut attributes.permissions,
            _ => return Err(ProtocolError::UnknownAttribute(name.to_string())),
        };
        if value.is_empty() {
            return Err(ProtocolError::MalformedMetadata(format!(
                "field '{name}' is empty"
            )));
        }
        if attribute.replace(value).is_some() {
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
    Ok(SandboxCapabilities::new(capabilities))
}

#[cfg(test)]
mod tests;
