use super::*;

#[test]
fn local_paths_extended_paths_and_volume_roots_query_the_same_volume() {
    let temp = std::env::temp_dir().canonicalize().unwrap();
    let extended = temp.to_string_lossy();
    let ordinary = PathBuf::from(extended.trim_prefix(r"\\?\"));
    let expected = query(&temp).unwrap();
    check(&temp).unwrap();
    let ordinary_volume = query(&ordinary).unwrap();
    assert_eq!(expected.flags, ordinary_volume.flags);
    assert_eq!(expected.filesystem, ordinary_volume.filesystem);
    let root = temp.ancestors().last().expect("missing volume root");
    let root_volume = query(root).unwrap();
    assert_eq!(expected.filesystem, root_volume.filesystem);
    assert_eq!(expected.flags, root_volume.flags);
}

#[test]
fn invalid_queries_cannot_default_to_the_current_volume() {
    for (path, reason) in [
        ("relative-workspace", "absolute path"),
        ("C:\\workspace\0suffix", "NUL"),
    ] {
        let message = query(Path::new(path))
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(message.contains(reason));
    }
}

#[test]
#[ignore = "requires KRAAI_TEST_NO_ACL_ROOT pointing to an existing filesystem without persistent ACLs"]
fn real_filesystem_without_acls_is_rejected() {
    let root = PathBuf::from(
        std::env::var_os("KRAAI_TEST_NO_ACL_ROOT").expect("set KRAAI_TEST_NO_ACL_ROOT"),
    );
    let volume = query(&root).unwrap();
    assert_eq!(volume.flags & FILE_PERSISTENT_ACLS, 0);
    let message = check(&root)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("FILE_PERSISTENT_ACLS"));
    assert!(message.contains(&root.display().to_string()));
    assert!(message.contains(&volume.filesystem));
    let mut plan = crate::LaunchPlan::new(
        std::env::current_exe().unwrap(),
        root.clone(),
        kraai_execution_types::SandboxCapabilities::new([
            kraai_execution_types::SandboxCapability::WorkspaceRead,
        ]),
        std::time::Duration::from_secs(10),
    );
    plan.runtime_roots.push(
        plan.executable
            .parent()
            .expect("missing executable directory")
            .to_path_buf(),
    );
    let temp = crate::temp_dir::PrivateTempDir::create(None).unwrap();
    let message = super::super::prepare(plan, temp)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("FILE_PERSISTENT_ACLS"));
    assert!(message.contains("configured session workspace root"));
    assert!(message.contains(&root.display().to_string()));
}
