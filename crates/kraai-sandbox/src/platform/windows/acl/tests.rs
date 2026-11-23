use super::*;

#[test]
fn wide_directory_stops_before_visiting_queued_children() {
    let _fixtures = lock_fixtures();
    let root =
        std::env::temp_dir().join(format!("kraai-acl-queue-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    for name in ["one", "two", "three"] {
        std::fs::write(root.join(name), b"content").unwrap();
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
    })
    .unwrap();
    assert_eq!(visited, 4);
    std::fs::remove_dir_all(root).unwrap();
}

fn lock_fixtures() -> std::sync::MutexGuard<'static, ()> {
    // Traversals pin shared ancestors, including Temp, against other fixtures' renames.
    static FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    FIXTURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn acl_setup_does_not_require_file_content_access() {
    let _fixtures = lock_fixtures();
    let identity = super::super::identity::Identity::create().unwrap();
    let root = std::env::temp_dir().join(format!(
        "kraai-acl-unreadable-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("unreadable");
    std::fs::write(&file, b"content").unwrap();
    let mut command = std::process::Command::new("icacls.exe");
    command.arg(&file).args(["/deny", "*S-1-1-0:(RD)"]);
    assert!(
        crate::spawn_command(&mut command)
            .unwrap()
            .wait()
            .unwrap()
            .success()
    );
    assert_eq!(
        File::open(&file).err().map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
    let mut grants = Grants::new(identity.sid.clone());
    for access in [Access::Read, Access::Write] {
        grants.grant(&root, access).unwrap();
        assert_eq!(grants.files.len(), 2);
        grants.cleanup().unwrap();
        assert!(File::open(&file).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hard_link_grants_are_rejected_before_mutation() {
    let _fixtures = lock_fixtures();
    let identity = super::super::identity::Identity::create().unwrap();
    let root =
        std::env::temp_dir().join(format!("kraai-acl-links-{:032x}", rand::random::<u128>()));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let external = root.join("secret");
    let link = workspace.join("link");
    std::fs::write(&external, b"host data").unwrap();
    std::fs::hard_link(&external, &link).unwrap();
    let mut grants = Grants::new(identity.sid.clone());
    for access in [Access::Read, Access::Write] {
        let result = grants.grant(&link, access);
        assert!(
            matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
        );
        assert!(grants.files.is_empty());
        grants.cleanup().unwrap();
        let result = grants.grant(&workspace, access);
        assert!(
            matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
        );
        grants.cleanup().unwrap();
    }
    std::fs::remove_file(link).unwrap();
    grants.grant(&workspace, Access::Write).unwrap();
    grants.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn traversal_pins_prevent_ancestor_replacement() {
    let _fixtures = lock_fixtures();
    let root = std::env::temp_dir().join(format!("kraai-acl-pins-{:032x}", rand::random::<u128>()));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("file"), b"content").unwrap();
    let mut visited = 0;
    visit_tree(&root, |_| {
        visited += 1;
        assert!(std::fs::rename(&root, root.with_extension("moved")).is_err());
        Ok(())
    })
    .unwrap();
    assert_eq!(visited, 3);
    let moved = root.with_extension("moved");
    std::fs::rename(&root, &moved).unwrap();
    std::fs::remove_dir_all(moved).unwrap();
}

#[test]
fn retained_grants_allow_renaming_existing_directories() {
    let _fixtures = lock_fixtures();
    let identity = super::super::identity::Identity::create().unwrap();
    let root =
        std::env::temp_dir().join(format!("kraai-acl-rename-{:032x}", rand::random::<u128>()));
    let original = root.join("original");
    let moved = root.join("moved");
    std::fs::create_dir_all(original.join("nested")).unwrap();
    std::fs::write(original.join("nested/file"), b"content").unwrap();
    let mut grants = Grants::new(identity.sid.clone());
    grants.grant(&original, Access::Write).unwrap();
    std::fs::rename(&original, &moved).unwrap();
    std::fs::rename(moved.join("nested"), moved.join("renamed")).unwrap();
    grants.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn entry_limit_preserves_cleanup_of_partial_grants() {
    let _fixtures = lock_fixtures();
    let identity = super::super::identity::Identity::create().unwrap();
    let root =
        std::env::temp_dir().join(format!("kraai-acl-limit-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    for name in ["one", "two", "three"] {
        std::fs::write(root.join(name), b"content").unwrap();
    }
    let mut grants = Grants::new(identity.sid.clone());
    let result = grants.grant_with_limit(&root, Access::Read, 2);
    assert!(
        matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("ACL entry limit (2) exceeded"))
    );
    assert_eq!(grants.files.len(), 2);
    grants.cleanup().unwrap();
    assert!(grants.files.is_empty());
    grants.grant_with_limit(&root, Access::Read, 4).unwrap();
    grants.cleanup().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn test_capability() -> Sid {
    super::super::identity::capability(&format!("kraai.test.{:032x}", rand::random::<u128>()))
        .unwrap()
        .into_iter()
        .next()
        .expect("Windows returned no capability SID")
}

#[test]
fn persistent_grants_are_applied_once_and_kept() {
    let _fixtures = lock_fixtures();
    let identity = super::super::identity::Identity::create().unwrap();
    let capability = test_capability();
    let root = std::env::temp_dir().join(format!(
        "kraai-acl-persistent-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("file"), b"content").unwrap();
    let pinned = || super::handle::open_by_id(&super::open(&root).unwrap()).unwrap();
    assert!(!super::mutation::grants(&pinned(), &capability, Access::Read).unwrap());
    grant_persistent(&root, &capability, Access::Read).unwrap();
    assert!(super::mutation::grants(&pinned(), &capability, Access::Read).unwrap());
    assert!(!super::mutation::grants(&pinned(), &capability, Access::Write).unwrap());
    assert!(!super::mutation::grants(&pinned(), &identity.sid, Access::Read).unwrap());
    grant_persistent(&root, &capability, Access::Read).unwrap();
    let mut grants = Grants::new(identity.sid.clone());
    grants.grant(&root, Access::Read).unwrap();
    grants.cleanup().unwrap();
    assert!(super::mutation::grants(&pinned(), &capability, Access::Read).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_persistent_grant_leaves_the_root_unmarked() {
    let _fixtures = lock_fixtures();
    let capability = test_capability();
    let root = std::env::temp_dir().join(format!(
        "kraai-acl-persistent-failure-{:032x}",
        rand::random::<u128>()
    ));
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("file"), b"content").unwrap();
    let external = root.join("secret");
    let link = workspace.join("link");
    std::fs::write(&external, b"host data").unwrap();
    std::fs::hard_link(&external, &link).unwrap();
    let pinned = || super::handle::open_by_id(&super::open(&workspace).unwrap()).unwrap();
    let result = grant_persistent(&workspace, &capability, Access::Read);
    assert!(
        matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message.contains("multiple hard links"))
    );
    assert!(!super::mutation::grants(&pinned(), &capability, Access::Read).unwrap());
    std::fs::remove_file(link).unwrap();
    grant_persistent(&workspace, &capability, Access::Read).unwrap();
    assert!(super::mutation::grants(&pinned(), &capability, Access::Read).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
