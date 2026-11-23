use super::*;
use std::fs;

#[test]
fn optional_reads_only_treat_missing_paths_as_absent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    assert!(read_optional(&path).unwrap().is_none());
    assert!(read_optional_text(&path).unwrap().is_none());
    assert!(read_optional(root.path()).is_err());
    assert!(read_optional_text(root.path()).is_err());
    fs::write(&path, [255]).unwrap();
    assert_eq!(read_optional(&path).unwrap(), Some(vec![255]));
    assert_eq!(
        read_optional_text(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        read_regular_text_file(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[cfg(unix)]
#[test]
fn optional_reads_follow_symlinks_but_reject_special_files() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("real"), b"inside").unwrap();
    symlink("real", root.path().join("link")).unwrap();
    assert_eq!(
        read_optional_text(&root.path().join("link")).unwrap(),
        Some(String::from("inside"))
    );
    symlink("/dev/zero", root.path().join("zero")).unwrap();
    assert_eq!(
        read_optional(&root.path().join("zero")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    let fifo = root.path().join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
    assert_eq!(
        read_optional_text(&fifo).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn final_symlink_policy_preserves_parent_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("directory")).unwrap();
    fs::write(root.path().join("directory/file"), b"inside").unwrap();
    symlink("directory", root.path().join("parent-link")).unwrap();
    symlink("file", root.path().join("directory/file-link")).unwrap();
    assert!(
        open_regular_file(
            &root.path().join("parent-link/file"),
            FinalSymlinkPolicy::Reject
        )
        .is_ok()
    );
    assert!(
        open_regular_file(
            &root.path().join("directory/file-link"),
            FinalSymlinkPolicy::Follow
        )
        .is_ok()
    );
    assert!(
        open_regular_file(
            &root.path().join("directory/file-link"),
            FinalSymlinkPolicy::Reject
        )
        .is_err()
    );
}

#[cfg(all(feature = "async", unix))]
#[tokio::test]
async fn asynchronous_regular_opens_reject_fifos_and_final_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRWXU).unwrap();
    assert!(
        open_regular_file_async(&path, FinalSymlinkPolicy::Reject)
            .await
            .is_err()
    );
    fs::remove_file(&path).unwrap();
    fs::write(root.path().join("real"), b"inside").unwrap();
    symlink("real", &path).unwrap();
    assert!(
        open_regular_file_async(&path, FinalSymlinkPolicy::Reject)
            .await
            .is_err()
    );
}
