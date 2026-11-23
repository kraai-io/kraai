use super::*;

#[test]
fn quotes_empty_whitespace_quotes_and_trailing_backslashes() {
    for (argument, expected) in [
        ("", "\"\""),
        ("one two", "\"one two\""),
        ("a\"b", "\"a\\\"b\""),
        ("tail\\", "\"tail\\\\\""),
        ("a\\\"b", "\"a\\\\\\\"b\""),
    ] {
        let mut encoded = Vec::new();
        assert!(quote(&mut encoded, OsStr::new(argument)).is_ok());
        assert_eq!(String::from_utf16_lossy(&encoded), expected);
    }
}

#[test]
fn environment_rejects_case_aliases_and_preserves_empty_block() {
    assert_eq!(environment(&BTreeMap::new()).ok(), Some(vec![0, 0]));
    let values = BTreeMap::from([
        (OsString::from("Path"), OsString::from("first")),
        (OsString::from("PATH"), OsString::from("second")),
    ]);
    assert!(environment(&values).is_err());
    assert!(wide(OsStr::new("nul\0value")).is_err());
}

#[test]
fn preserves_hidden_environment_entries() {
    let values = BTreeMap::from([
        (OsString::from("=C:"), OsString::from(r"C:\work")),
        (OsString::from("=D:"), OsString::from(r"D:\other")),
        (OsString::from("=ExitCode"), OsString::from("0")),
        (OsString::from("Path"), OsString::from(r"C:\Windows")),
    ]);
    assert_eq!(
        environment(&values)
            .map(|block| String::from_utf16_lossy(&block))
            .ok(),
        Some("=C:=C:\\work\0=D:=D:\\other\0=ExitCode=0\0Path=C:\\Windows\0\0".into()),
    );
    let aliases = BTreeMap::from([
        (OsString::from("=C:"), OsString::from("first")),
        (OsString::from("=c:"), OsString::from("second")),
    ]);
    assert!(environment(&aliases).is_err());
}

#[test]
fn rejects_empty_names_embedded_equals_and_nuls() {
    for name in ["", "=", "A=B", "=C:=bad", "==C:", "nul\0name"] {
        let values = BTreeMap::from([(OsString::from(name), OsString::from("value"))]);
        assert!(environment(&values).is_err(), "accepted {name:?}");
    }
    let values = BTreeMap::from([(OsString::from("=C:"), OsString::from("nul\0value"))]);
    assert!(environment(&values).is_err());
}

#[test]
fn rejects_shell_scripts_and_invalid_command_lines() {
    assert!(application(Path::new(r"C:\tools\script.CmD")).is_err());
    assert!(application(Path::new(r"C:\tools\script.bat")).is_err());
    assert!(application(Path::new("relative.exe")).is_err());
    assert!(
        arguments(
            Path::new(r"C:\tool.exe"),
            &[OsString::from("nul\0argument")]
        )
        .is_err()
    );
    assert!(
        arguments(
            Path::new(r"C:\tool.exe"),
            &[OsString::from("a".repeat(32767))]
        )
        .is_err()
    );
}

#[test]
fn preserves_environment_values_and_orders_names() {
    let values = BTreeMap::from([
        (OsString::from("zed"), OsString::from("spaces and = signs")),
        (OsString::from("Alpha"), OsString::from("\u{1f426}")),
    ]);
    assert_eq!(
        environment(&values)
            .map(|block| String::from_utf16_lossy(&block))
            .ok(),
        Some(String::from("Alpha=\u{1f426}\0zed=spaces and = signs\0\0"))
    );
}
