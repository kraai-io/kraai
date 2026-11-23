use super::*;

#[test]
fn retry_syncs_ancestor_entries_left_by_failed_directory_creation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("parent/child");
    let failed = create_directories_with_sync(root.path(), &path, false, |directory| {
        if directory == root.path() {
            Err(io::Error::other("injected ancestor sync failure"))
        } else {
            Ok(())
        }
    });
    assert!(failed.is_err());
    assert!(path.is_dir());
    let mut synced = Vec::new();
    create_directories_with_sync(root.path(), &path, false, |directory| {
        synced.push(directory.to_path_buf());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        synced,
        vec![path, root.path().join("parent"), root.path().to_path_buf()]
    );
}

#[test]
fn anchored_creation_rejects_escape_and_missing_anchor() {
    let root = tempfile::tempdir().unwrap();
    let anchor = root.path().join("anchor");
    assert!(create_dir_all_in(&anchor, &anchor.join("child")).is_err());
    assert!(!anchor.exists());
    fs::create_dir(&anchor).unwrap();
    for path in [root.path().join("outside"), anchor.join("../outside")] {
        assert_eq!(
            create_dir_all_in(&anchor, &path)
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::InvalidInput)
        );
    }
    assert!(!root.path().join("outside").exists());
}

#[test]
fn relative_directory_chains_stop_at_the_current_directory() {
    for anchor in [Path::new(""), Path::new(".")] {
        assert_eq!(
            directory_chain(anchor, Path::new("parent/child")).unwrap(),
            vec![
                PathBuf::from("."),
                PathBuf::from("./parent"),
                PathBuf::from("./parent/child")
            ]
        );
        assert_eq!(
            directory_chain(anchor, Path::new(".")).unwrap(),
            vec![PathBuf::from(".")]
        );
    }
}

#[cfg(unix)]
#[test]
fn bootstrap_and_anchored_creation_allow_execute_only_ancestors() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let ancestor = root.path().join("search-only");
    let anchor = ancestor.join("storage");
    fs::create_dir_all(&anchor).unwrap();
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o111)).unwrap();
    let bootstrap = create_dir_all(&anchor);
    let nested = create_dir_all_in(&anchor, &anchor.join("nested/child"));
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
    bootstrap.unwrap();
    nested.unwrap();
    assert!(anchor.join("nested/child").is_dir());
}

#[cfg(unix)]
#[test]
fn syncing_rejects_regular_files_and_fifos_without_blocking() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    fs::write(&file, b"value").unwrap();
    assert_eq!(
        sync_directory(&file).err().map(|error| error.kind()),
        Some(io::ErrorKind::NotADirectory)
    );
    let fifo = root.path().join("fifo");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    assert_eq!(
        sync_directory(&fifo).err().map(|error| error.kind()),
        Some(io::ErrorKind::NotADirectory)
    );
}

#[test]
fn durable_removal_can_be_retried_after_the_entry_disappears() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    fs::write(&path, b"value").unwrap();
    assert!(remove_file_durable(&path).unwrap());
    assert!(!remove_file_durable(&path).unwrap());
    let directory = root.path().join("directory");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("nested"), b"value").unwrap();
    remove_dir_all_durable(&directory).unwrap();
    remove_dir_all_durable(&directory).unwrap();
}
