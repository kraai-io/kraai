use super::*;

#[test]
fn failed_create_preserves_existing_file_and_cleans_temporary() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    atomic_create(&path, b"original")
        .unwrap()
        .into_result()
        .unwrap();
    let error = atomic_create(&path, b"replacement").expect_err("creation replaced existing file");
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn failed_replacement_removes_only_owned_temporary_file() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    let temporary = root.path().join("temporary");
    fs::write(&temporary, b"another writer").unwrap();
    let error = write_with_sync(
        &destination,
        b"new",
        WriteMode::Replace,
        true,
        &temporary,
        sync_directory,
    )
    .expect_err("temporary file was reused");
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&temporary).unwrap(), b"another writer");
    fs::remove_file(&temporary).unwrap();
    assert!(
        write_with_sync(
            &destination,
            b"new",
            WriteMode::Replace,
            true,
            &temporary,
            sync_directory
        )
        .is_err()
    );
    assert!(!temporary.try_exists().unwrap());
    assert!(destination.is_dir());
}

#[test]
fn directory_sync_failure_exposes_publication_without_removing_reused_temporary() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    let temporary = root.path().join("temporary");
    let outcome = write_with_sync(
        &path,
        b"published",
        WriteMode::Create,
        true,
        &temporary,
        |_| {
            fs::write(&temporary, b"another writer")?;
            Err(io::Error::other("injected directory sync failure"))
        },
    )
    .unwrap();
    assert!(matches!(
        outcome,
        AtomicWriteOutcome::ReplacedButNotSynced(_)
    ));
    assert_eq!(fs::read(&path).unwrap(), b"published");
    assert_eq!(fs::read(&temporary).unwrap(), b"another writer");
}

#[test]
fn unanchored_writes_require_existing_parent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing/state");
    assert!(atomic_create(&path, b"value").is_err());
    assert!(atomic_replace(&path, b"value").is_err());
    assert!(atomic_replace_private(&path, b"value").is_err());
    assert!(!path.parent().is_some_and(Path::exists));
    atomic_replace_in(root.path(), &path, b"value")
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), b"value");
}

#[cfg(unix)]
#[test]
fn private_temporary_file_is_restricted_before_writing_credentials() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    for (name, mode) in [
        ("private", WriteMode::Private),
        (
            "preserved",
            WriteMode::Preserve(Permissions::from_mode(0o644)),
        ),
    ] {
        let path = root.path().join(name);
        let file = open_temporary(&path, &mode).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(file.metadata().unwrap().len(), 0);
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn private_replacement_and_preserved_modes_are_distinct() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private/credentials");
    atomic_replace_private_in(root.path(), &path, b"secret")
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        root.path()
            .join("private")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    atomic_replace_preserving(&path, b"executable", Permissions::from_mode(0o751))
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o751);
    atomic_replace_private(&path, b"new secret")
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    atomic_replace_private_unsynced(&path, b"ephemeral secret").unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read(&path).unwrap(), b"ephemeral secret");
}

#[cfg(unix)]
#[test]
fn maximum_length_destination_uses_short_independent_temporary_name() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("x".repeat(255));
    atomic_replace(&path, b"old")
        .unwrap()
        .into_result()
        .unwrap();
    atomic_replace(&path, b"new")
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn replacement_does_not_open_ancestors_above_the_anchor() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let ancestor = root.path().join("search-only");
    let anchor = ancestor.join("storage");
    fs::create_dir_all(&anchor).unwrap();
    fs::set_permissions(&ancestor, Permissions::from_mode(0o111)).unwrap();
    let direct = atomic_replace(&anchor.join("direct"), b"direct");
    let nested = atomic_replace_in(&anchor, &anchor.join("nested/state"), b"nested");
    fs::set_permissions(&ancestor, Permissions::from_mode(0o700)).unwrap();
    direct.unwrap().into_result().unwrap();
    nested.unwrap().into_result().unwrap();
    assert_eq!(fs::read(anchor.join("direct")).unwrap(), b"direct");
    assert_eq!(fs::read(anchor.join("nested/state")).unwrap(), b"nested");
}

#[cfg(unix)]
#[test]
fn anchored_replacement_preserves_symlinked_storage_roots() {
    let root = tempfile::tempdir().unwrap();
    let storage = root.path().join("storage");
    let anchor = root.path().join("alias");
    fs::create_dir(&storage).unwrap();
    std::os::unix::fs::symlink(&storage, &anchor).unwrap();
    atomic_replace_in(&anchor, &anchor.join("nested/state"), b"value")
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(fs::read(storage.join("nested/state")).unwrap(), b"value");
}

#[test]
fn unsynced_cache_replacement_does_not_call_directory_sync() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cache");
    for mode in [WriteMode::Replace, WriteMode::Private] {
        write_with_sync(
            &path,
            b"cache",
            mode,
            false,
            &temporary_path(&path).unwrap(),
            |_| Err(io::Error::other("must not sync")),
        )
        .unwrap()
        .into_result()
        .unwrap();
    }
    assert_eq!(fs::read(path).unwrap(), b"cache");
}

#[cfg(feature = "async")]
#[tokio::test]
async fn asynchronous_replacement_publishes_complete_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    atomic_replace_async(&path, b"complete")
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), b"complete");
    let nested = root.path().join("nested/state");
    atomic_replace_in_async(root.path(), &nested, b"nested")
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(fs::read(nested).unwrap(), b"nested");
}

#[cfg(windows)]
#[test]
fn private_writes_get_a_protected_owner_only_acl_on_windows() {
    fn dacl(path: &Path) -> String {
        let sddl = crate::fs::owner_only::dacl_sddl(path).unwrap();
        sddl.split_once("D:")
            .map(|(_, dacl)| dacl.to_owned())
            .unwrap_or_else(|| panic!("no DACL in {sddl:?}"))
    }

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private/credentials");
    atomic_replace_private_in(root.path(), &path, b"secret")
        .unwrap()
        .into_result()
        .unwrap();
    atomic_replace_private(&path, b"new secret")
        .unwrap()
        .into_result()
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new secret");
    for (entry, flags) in [(root.path().join("private"), "OICI"), (path, "")] {
        let dacl = dacl(&entry);
        assert!(dacl.starts_with("P"), "{dacl} inherits from its parent");
        assert_eq!(dacl.matches("(A;").count(), 2, "{dacl} has extra grants");
        assert!(dacl.contains(&format!("(A;{flags};FA;;;SY)")), "{dacl}");
    }
}
