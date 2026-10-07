use super::*;

#[test]
fn failed_create_preserves_existing_file_and_cleans_temporary() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state");
    atomic_create(&path, b"original")?.into_result()?;
    let error = atomic_create(&path, b"replacement")
        .err()
        .ok_or_else(|| io::Error::other("creation replaced existing file"))?;
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&path)?, b"original");
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[test]
fn failed_replacement_removes_only_owned_temporary_file() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let destination = root.path().join("destination");
    fs::create_dir(&destination)?;
    let temporary = root.path().join("temporary");
    fs::write(&temporary, b"another writer")?;
    let error = write_with_sync(
        &destination,
        b"new",
        WriteMode::Replace,
        true,
        &temporary,
        sync_directory,
    )
    .err()
    .ok_or_else(|| io::Error::other("temporary file was reused"))?;
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&temporary)?, b"another writer");
    fs::remove_file(&temporary)?;
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
    assert!(!temporary.try_exists()?);
    assert!(destination.is_dir());
    Ok(())
}

#[test]
fn directory_sync_failure_exposes_publication_without_removing_reused_temporary() -> io::Result<()>
{
    let root = tempfile::tempdir()?;
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
    )?;
    assert!(matches!(
        outcome,
        AtomicWriteOutcome::ReplacedButNotSynced(_)
    ));
    assert_eq!(fs::read(&path)?, b"published");
    assert_eq!(fs::read(&temporary)?, b"another writer");
    Ok(())
}

#[test]
fn unanchored_writes_require_existing_parent() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("missing/state");
    assert!(atomic_create(&path, b"value").is_err());
    assert!(atomic_replace(&path, b"value").is_err());
    assert!(atomic_replace_private(&path, b"value").is_err());
    assert!(!path.parent().is_some_and(Path::exists));
    atomic_replace_in(root.path(), &path, b"value")?.into_result()?;
    assert_eq!(fs::read(path)?, b"value");
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_temporary_file_is_restricted_before_writing_credentials() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir()?;
    let path = root.path().join("temporary");
    let file = open_temporary(&path, &WriteMode::Private)?;
    assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
    assert_eq!(file.metadata()?.len(), 0);
    assert_eq!(path.metadata()?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[cfg(unix)]
#[test]
fn private_replacement_and_preserved_modes_are_distinct() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir()?;
    let path = root.path().join("private/credentials");
    atomic_replace_private_in(root.path(), &path, b"secret")?.into_result()?;
    assert_eq!(path.metadata()?.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        root.path().join("private").metadata()?.permissions().mode() & 0o777,
        0o700
    );
    atomic_replace_preserving(&path, b"executable", Permissions::from_mode(0o751))?
        .into_result()?;
    assert_eq!(path.metadata()?.permissions().mode() & 0o777, 0o751);
    atomic_replace_private(&path, b"new secret")?.into_result()?;
    assert_eq!(path.metadata()?.permissions().mode() & 0o777, 0o600);
    atomic_replace_private_unsynced(&path, b"ephemeral secret")?;
    assert_eq!(path.metadata()?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read(&path)?, b"ephemeral secret");
    Ok(())
}

#[cfg(unix)]
#[test]
fn maximum_length_destination_uses_short_independent_temporary_name() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("x".repeat(255));
    atomic_replace(&path, b"old")?.into_result()?;
    atomic_replace(&path, b"new")?.into_result()?;
    assert_eq!(fs::read(&path)?, b"new");
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn replacement_does_not_open_ancestors_above_the_anchor() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir()?;
    let ancestor = root.path().join("search-only");
    let anchor = ancestor.join("storage");
    fs::create_dir_all(&anchor)?;
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o111))?;
    let direct = atomic_replace(&anchor.join("direct"), b"direct");
    let nested = atomic_replace_in(&anchor, &anchor.join("nested/state"), b"nested");
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700))?;
    direct?.into_result()?;
    nested?.into_result()?;
    assert_eq!(fs::read(anchor.join("direct"))?, b"direct");
    assert_eq!(fs::read(anchor.join("nested/state"))?, b"nested");
    Ok(())
}

#[cfg(unix)]
#[test]
fn anchored_replacement_preserves_symlinked_storage_roots() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let storage = root.path().join("storage");
    let anchor = root.path().join("alias");
    fs::create_dir(&storage)?;
    std::os::unix::fs::symlink(&storage, &anchor)?;
    atomic_replace_in(&anchor, &anchor.join("nested/state"), b"value")?.into_result()?;
    assert_eq!(fs::read(storage.join("nested/state"))?, b"value");
    Ok(())
}

#[test]
fn unsynced_cache_replacement_does_not_call_directory_sync() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("cache");
    for mode in [WriteMode::Replace, WriteMode::Private] {
        write_with_sync(
            &path,
            b"cache",
            mode,
            false,
            &temporary_path(&path)?,
            |_| Err(io::Error::other("must not sync")),
        )?
        .into_result()?;
    }
    assert_eq!(fs::read(path)?, b"cache");
    Ok(())
}

#[cfg(feature = "async")]
#[tokio::test]
async fn asynchronous_replacement_publishes_complete_file() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state");
    atomic_replace_async(&path, b"complete")
        .await?
        .into_result()?;
    assert_eq!(fs::read(path)?, b"complete");
    let nested = root.path().join("nested/state");
    atomic_replace_in_async(root.path(), &nested, b"nested")
        .await?
        .into_result()?;
    assert_eq!(fs::read(nested)?, b"nested");
    Ok(())
}
