use super::*;

fn volume(name: &str, flags: u32) -> Volume {
    Volume {
        filesystem: name.into(),
        flags,
    }
}

#[test]
fn unsupported_capabilities_name_the_path_filesystem_and_remedy() {
    let workspace = Path::new(r"X:\kraai-Windows");
    let result = check_with(workspace, |_| Ok(volume("exFAT", 0)));
    let message = result
        .err()
        .map(|error| error.with_workspace_root(workspace).to_string())
        .unwrap_or_default();
    for text in [
        r"X:\kraai-Windows",
        "exFAT",
        "FILE_PERSISTENT_ACLS",
        "FILE_SUPPORTS_OPEN_BY_FILE_ID",
        "Move or reconfigure",
        "configured session workspace root",
        "sandboxing remains required",
    ] {
        assert!(message.contains(text), "missing {text}: {message}");
    }
}

#[test]
fn capability_flags_determine_support_instead_of_filesystem_names() {
    for name in ["NTFS", "ReFS", "other", ""] {
        assert!(
            check_with(Path::new(r"C:\workspace"), |_| Ok(volume(
                name,
                FILE_PERSISTENT_ACLS | FILE_SUPPORTS_OPEN_BY_FILE_ID
            )))
            .is_ok()
        );
    }
    for (flags, missing, present) in [
        (
            FILE_PERSISTENT_ACLS,
            "FILE_SUPPORTS_OPEN_BY_FILE_ID",
            "FILE_PERSISTENT_ACLS",
        ),
        (
            FILE_SUPPORTS_OPEN_BY_FILE_ID,
            "FILE_PERSISTENT_ACLS",
            "FILE_SUPPORTS_OPEN_BY_FILE_ID",
        ),
    ] {
        let message = check_with(Path::new(r"C:\workspace"), |_| Ok(volume("", flags)))
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(message.contains(missing));
        assert!(!message.contains(present));
        assert!(message.contains("unknown"));
    }
}

#[test]
fn every_secured_root_is_checked_and_failure_stops_setup() {
    let workspace = Path::new(r"X:\session-workspace");
    let runtime = PathBuf::from(r"C:\installed-kraai");
    let temp = Path::new(r"T:\private-temp");
    for failing in [workspace, runtime.as_path(), temp] {
        let mut seen = Vec::new();
        let result = check_roots_with(workspace, std::slice::from_ref(&runtime), temp, |path| {
            seen.push(path.to_path_buf());
            Ok(volume(
                if path == failing { "exFAT" } else { "NTFS" },
                if path == failing {
                    0
                } else {
                    FILE_PERSISTENT_ACLS | FILE_SUPPORTS_OPEN_BY_FILE_ID
                },
            ))
        });
        let message = result
            .err()
            .map(|error| error.with_workspace_root(workspace).to_string())
            .unwrap_or_default();
        assert!(message.contains(&failing.display().to_string()));
        assert!(message.contains(&workspace.display().to_string()));
        assert_eq!(seen.last().map(PathBuf::as_path), Some(failing));
    }
    let mut seen = Vec::new();
    assert!(
        check_roots_with(workspace, std::slice::from_ref(&runtime), temp, |path| {
            seen.push(path.to_path_buf());
            Ok(volume(
                "NTFS",
                FILE_PERSISTENT_ACLS | FILE_SUPPORTS_OPEN_BY_FILE_ID,
            ))
        })
        .is_ok()
    );
    assert_eq!(seen, [workspace.to_path_buf(), runtime, temp.to_path_buf()]);
}

#[test]
fn queries_preserve_path_forms_and_api_failure_context() {
    for path in [
        r"X:\",
        r"X:\workspace",
        r"\\?\X:\",
        r"\\?\X:\workspace",
        r"\\server\share\",
        r"\\?\UNC\server\share\workspace",
        r"C:\mounted-volume\workspace",
    ] {
        let path = Path::new(path);
        let result = check_with(path, |actual| {
            assert_eq!(actual, path);
            Err(query_error(
                actual,
                "GetVolumeInformationW",
                std::io::Error::from_raw_os_error(87),
            ))
        });
        let message = result
            .err()
            .map(|error| {
                error
                    .with_workspace_root(Path::new(r"F:\workspace"))
                    .to_string()
            })
            .unwrap_or_default();
        for text in [
            path.to_str().unwrap_or_default(),
            "GetVolumeInformationW",
            "87",
            r"F:\workspace",
        ] {
            assert!(message.contains(text), "missing {text}: {message}");
        }
        assert!(!message.contains("lacks"));
    }
}
