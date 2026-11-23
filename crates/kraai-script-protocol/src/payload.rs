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
            "# timeout=1.5sec permissions=workspace-write,network\nls | get name",
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
    fn no_sandbox_may_be_combined_with_other_permissions() {
        let script =
            parse_script_input("# permissions=network,no-sandbox\nls").expect("valid script input");
        assert!(script.requested_capabilities.is_unsandboxed());
    }

    #[test]
    fn scripts_without_metadata_use_defaults_and_keep_comments() {
        for (input, source) in [
            ("ls", "ls"),
            ("\n  ls | get name", "ls | get name"),
            ("# list files\nls", "# list files\nls"),
            ("#\nls", "#\nls"),
            ("#!/usr/bin/env nu\nls", "#!/usr/bin/env nu\nls"),
        ] {
            let script = parse_script_input(input).expect("valid script input");
            assert_eq!(script.timeout, DEFAULT_SCRIPT_TIMEOUT, "{input:?}");
            assert_eq!(script.source, source.as_bytes(), "{input:?}");
            assert_eq!(
                script.requested_capabilities,
                SandboxCapabilities::default()
            );
        }
        let script = parse_script_input("# permissions=network\nls").expect("valid script input");
        assert_eq!(script.timeout, DEFAULT_SCRIPT_TIMEOUT);
        assert_eq!(script.source, b"ls");
        assert!(
            script
                .requested_capabilities
                .contains(SandboxCapability::Network)
        );
    }

    #[test]
    fn splits_the_header_off_whole_and_partial_scripts() {
        for (input, fields, source) in [
            ("# timeout=5min\nls\n", Some(" timeout=5min"), "ls\n"),
            ("\n\n# timeout=5min\r\n\r\nls", Some(" timeout=5min"), "ls"),
            ("# timeout=", Some(" timeout="), ""),
            ("# timeo", None, "# timeo"),
            ("# list files\nls", None, "# list files\nls"),
            ("#!/usr/bin/env nu\nls", None, "#!/usr/bin/env nu\nls"),
            ("\n  ls", None, "ls"),
            (" \n", None, ""),
        ] {
            assert_eq!(split_script_header(input), (fields, source), "{input:?}");
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_metadata() {
        assert!(parse_script_input("# kraai timeout=1sec\necho hi").is_err());
        assert!(parse_script_input("# timeout=1sec timeout=2sec\necho hi").is_err());
        assert!(parse_script_input("# timeout=1sec mystery=x\necho hi").is_err());
    }

    #[test]
    fn caps_the_timeout_at_one_day() {
        let script = parse_script_input("# timeout=1day\nls").expect("valid script input");
        assert_eq!(script.timeout, MAX_SCRIPT_TIMEOUT);
        for input in [
            "# timeout=24.5hr\nls",
            "# timeout=1wk\nls",
            "# timeout=10000000000000000000sec\nls",
        ] {
            assert_eq!(
                parse_script_input(input),
                Err(ProtocolError::InvalidTimeout(String::from(
                    "duration must be at most 1day"
                ))),
                "{input:?}"
            );
        }
    }

    #[test]
    fn metadata_errors_preserve_field_and_validation_order() {
        for (input, expected) in [
            (
                "# timeout=invalid timeout=\necho hi",
                ProtocolError::MalformedMetadata(String::from("field 'timeout' is empty")),
            ),
            (
                "# timeout=invalid unknown=\necho hi",
                ProtocolError::UnknownAttribute(String::from("unknown")),
            ),
            (
                "# timeout=invalid timeout=1sec\necho hi",
                ProtocolError::DuplicateAttribute(String::from("timeout")),
            ),
            (
                "# permissions=invalid permissions=network timeout=1sec\necho hi",
                ProtocolError::DuplicateAttribute(String::from("permissions")),
            ),
            (
                "# permissions=invalid timeout=10secs\necho hi",
                ProtocolError::InvalidTimeout(String::from("unknown Nushell duration unit 'secs'")),
            ),
            (
                "# permissions=invalid\necho hi",
                ProtocolError::InvalidPermissions(String::from("unknown capability 'invalid'")),
            ),
            (
                "# timeout=invalid missing\necho hi",
                ProtocolError::MalformedMetadata(String::from("field 'missing' is missing '='")),
            ),
            (
                "# timeout=1sec permissions=unknown\n \t",
                ProtocolError::InvalidPermissions(String::from("unknown capability 'unknown'")),
            ),
        ] {
            assert_eq!(parse_script_input(input), Err(expected), "{input:?}");
        }
    }

    #[test]
    fn merged_scripts_run_in_order_with_summed_timeouts_and_every_permission() {
        let scripts = [
            "# timeout=2min permissions=network\nls\n\n",
            "git status",
            "# permissions=workspace-write\njj st",
        ]
        .map(|input| parse_script_input(input).expect("valid script input"));
        let merged = merge_scripts(&scripts);
        assert_eq!(
            merged,
            "# timeout=3min permissions=workspace-write,network\ndo {\nls\n}\n\ndo {\ngit status\n}\n\ndo {\njj st\n}"
        );
        let reparsed = parse_script_input(&merged).expect("merged script parses");
        assert_eq!(reparsed.timeout, Duration::from_secs(180));
    }

    #[test]
    fn merged_default_scripts_keep_a_header_for_the_longer_timeout() {
        let scripts =
            ["ls", "pwd"].map(|input| parse_script_input(input).expect("valid script input"));
        assert_eq!(
            merge_scripts(&scripts),
            "# timeout=1min\ndo {\nls\n}\n\ndo {\npwd\n}"
        );
    }
}
