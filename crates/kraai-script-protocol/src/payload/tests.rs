use super::*;

#[test]
fn parses_universal_nushell_metadata_header() {
    let script =
        parse_script_input("# timeout=1.5sec permissions=workspace-write,network\nls | get name")
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
    let scripts = ["ls", "pwd"].map(|input| parse_script_input(input).expect("valid script input"));
    assert_eq!(
        merge_scripts(&scripts),
        "# timeout=1min\ndo {\nls\n}\n\ndo {\npwd\n}"
    );
}
