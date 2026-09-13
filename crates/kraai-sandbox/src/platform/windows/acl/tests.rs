use super::*;

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression tests assert traversal limits"
)]
fn wide_directory_stops_before_visiting_queued_children() -> Result<(), Box<dyn std::error::Error>>
{
    let _fixtures = lock_fixtures()?;
    let root =
        std::env::temp_dir().join(format!("kraai-acl-queue-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root)?;
    for name in ["one", "two", "three"] {
        std::fs::write(root.join(name), b"content")?;
    }
    let mut visited = 0;
    let result = visit_tree_with_limit(&root, 3, |_| {
        visited += 1;
        Ok(())
    });
    assert!(
        matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("ACL entry limit (3) exceeded"))
    );
    assert_eq!(visited, 1);
    visited = 0;
    visit_tree_with_limit(&root, 4, |_| {
        visited += 1;
        Ok(())
    })?;
    assert_eq!(visited, 4);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn lock_fixtures() -> std::io::Result<std::sync::MutexGuard<'static, ()>> {
    // Traversals pin shared ancestors, including Temp, against other fixtures' renames.
    static FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    FIXTURES
        .lock()
        .map_err(|error| std::io::Error::other(format!("ACL fixture lock poisoned: {error}")))
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression tests assert permission boundaries"
)]
fn acl_setup_does_not_require_file_content_access() -> Result<(), Box<dyn std::error::Error>> {
    let _fixtures = lock_fixtures()?;
    let identity = super::super::identity::Identity::create()?;
    let root = std::env::temp_dir().join(format!(
        "kraai-acl-unreadable-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root)?;
    let file = root.join("unreadable");
    std::fs::write(&file, b"content")?;
    let mut command = std::process::Command::new("icacls.exe");
    command.arg(&file).args(["/deny", "*S-1-1-0:(RD)"]);
    assert!(crate::spawn_command(&mut command)?.wait()?.success());
    assert_eq!(
        File::open(&file).err().map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
    let mut grants = Grants::new(identity.sid.bytes());
    for access in [Access::Read, Access::Write] {
        grants.grant(&root, access)?;
        assert_eq!(grants.files.len(), 2);
        grants.cleanup()?;
        assert!(File::open(&file).is_err());
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression tests assert permission boundaries"
)]
fn hard_link_grants_are_rejected_before_mutation() -> Result<(), Box<dyn std::error::Error>> {
    let _fixtures = lock_fixtures()?;
    let identity = super::super::identity::Identity::create()?;
    let root =
        std::env::temp_dir().join(format!("kraai-acl-links-{:032x}", rand::random::<u128>()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let external = root.join("secret");
    let link = workspace.join("link");
    std::fs::write(&external, b"host data")?;
    std::fs::hard_link(&external, &link)?;
    let mut grants = Grants::new(identity.sid.bytes());
    for access in [Access::Read, Access::Write] {
        let result = grants.grant(&link, access);
        assert!(
            matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
        );
        assert!(grants.files.is_empty());
        grants.cleanup()?;
        let result = grants.grant(&workspace, access);
        assert!(
            matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
        );
        grants.cleanup()?;
    }
    std::fs::remove_file(link)?;
    grants.grant(&workspace, Access::Write)?;
    grants.cleanup()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression tests assert permission boundaries"
)]
fn traversal_pins_prevent_ancestor_replacement() -> Result<(), Box<dyn std::error::Error>> {
    let _fixtures = lock_fixtures()?;
    let root = std::env::temp_dir().join(format!("kraai-acl-pins-{:032x}", rand::random::<u128>()));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested)?;
    std::fs::write(nested.join("file"), b"content")?;
    let mut visited = 0;
    visit_tree(&root, |_| {
        visited += 1;
        assert!(std::fs::rename(&root, root.with_extension("moved")).is_err());
        Ok(())
    })?;
    assert_eq!(visited, 3);
    let moved = root.with_extension("moved");
    std::fs::rename(&root, &moved)?;
    std::fs::remove_dir_all(moved)?;
    Ok(())
}

#[test]
fn retained_grants_allow_renaming_existing_directories() -> Result<(), Box<dyn std::error::Error>> {
    let _fixtures = lock_fixtures()?;
    let identity = super::super::identity::Identity::create()?;
    let root =
        std::env::temp_dir().join(format!("kraai-acl-rename-{:032x}", rand::random::<u128>()));
    let original = root.join("original");
    let moved = root.join("moved");
    std::fs::create_dir_all(original.join("nested"))?;
    std::fs::write(original.join("nested/file"), b"content")?;
    let mut grants = Grants::new(identity.sid.bytes());
    grants.grant(&original, Access::Write)?;
    std::fs::rename(&original, &moved)?;
    std::fs::rename(moved.join("nested"), moved.join("renamed"))?;
    grants.cleanup()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression tests assert observable behavior"
)]
fn entry_limit_preserves_cleanup_of_partial_grants() -> Result<(), Box<dyn std::error::Error>> {
    let _fixtures = lock_fixtures()?;
    let identity = super::super::identity::Identity::create()?;
    let root =
        std::env::temp_dir().join(format!("kraai-acl-limit-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root)?;
    for name in ["one", "two", "three"] {
        std::fs::write(root.join(name), b"content")?;
    }
    let mut grants = Grants::new(identity.sid.bytes());
    let result = grants.grant_with_limit(&root, Access::Read, 2);
    assert!(
        matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("ACL entry limit (2) exceeded"))
    );
    assert_eq!(grants.files.len(), 2);
    grants.cleanup()?;
    assert!(grants.files.is_empty());
    grants.grant_with_limit(&root, Access::Read, 4)?;
    grants.cleanup()?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}
