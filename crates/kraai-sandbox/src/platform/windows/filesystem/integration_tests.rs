#![expect(
    clippy::panic_in_result_fn,
    reason = "integration tests assert Windows volume query results"
)]

use super::*;

#[test]
fn local_paths_extended_paths_and_volume_roots_query_the_same_volume()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = std::env::temp_dir().canonicalize()?;
    let extended = temp.to_string_lossy();
    let ordinary = PathBuf::from(extended.strip_prefix(r"\\?\").unwrap_or(&extended));
    let expected = query(&temp)?;
    check(&temp)?;
    let ordinary_volume = query(&ordinary)?;
    assert_eq!(expected.flags, ordinary_volume.flags);
    assert_eq!(expected.filesystem, ordinary_volume.filesystem);
    let root = temp.ancestors().last().ok_or("missing volume root")?;
    let root_volume = query(root)?;
    assert_eq!(expected.filesystem, root_volume.filesystem);
    assert_eq!(expected.flags, root_volume.flags);
    Ok(())
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
fn real_filesystem_without_acls_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::var_os("KRAAI_TEST_NO_ACL_ROOT").ok_or("set KRAAI_TEST_NO_ACL_ROOT")?,
    );
    let volume = query(&root)?;
    assert_eq!(volume.flags & FILE_PERSISTENT_ACLS, 0);
    let message = check(&root)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("FILE_PERSISTENT_ACLS"));
    assert!(message.contains(&root.display().to_string()));
    assert!(message.contains(&volume.filesystem));
    let mut plan = crate::LaunchPlan::new(
        std::env::current_exe()?,
        root.clone(),
        kraai_types::SandboxCapabilities::workspace_read(),
        std::time::Duration::from_secs(10),
    );
    plan.runtime_roots.push(
        plan.executable
            .parent()
            .ok_or("missing executable directory")?
            .to_path_buf(),
    );
    let temp = crate::temp_dir::PrivateTempDir::create(None)?;
    let message = super::super::prepare(plan, temp)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(message.contains("FILE_PERSISTENT_ACLS"));
    assert!(message.contains("configured session workspace root"));
    assert!(message.contains(&root.display().to_string()));
    Ok(())
}

#[test]
#[ignore = "requires KRAAI_TEST_UNC_ROOT pointing to an accessible network share; does not test sandbox support"]
fn unc_share_query_preserves_volume_information_or_api_error()
-> Result<(), Box<dyn std::error::Error>> {
    let root =
        PathBuf::from(std::env::var_os("KRAAI_TEST_UNC_ROOT").ok_or("set KRAAI_TEST_UNC_ROOT")?);
    let ordinary = root.to_string_lossy();
    let tail = ordinary
        .strip_prefix(r"\\")
        .ok_or("expected ordinary UNC path")?;
    let extended = PathBuf::from(format!(r"\\?\UNC\{tail}"));
    for path in [&root, &extended] {
        match query(path) {
            Ok(volume) => assert!(!volume.filesystem.is_empty()),
            Err(error) => {
                let message = error.to_string();
                assert!(message.contains("GetVolume"));
                assert!(message.contains(&path.display().to_string()));
            }
        }
    }
    Ok(())
}
