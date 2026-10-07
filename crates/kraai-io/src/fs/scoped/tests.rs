#![expect(
    clippy::unwrap_used,
    reason = "scoped filesystem tests assert fixture operations"
)]

use std::fs;
use std::io::Read;

use super::*;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn opened_directory_survives_path_replacement() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    let moved = parent.path().join("moved");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "authorized").unwrap();
    let scope = ScopedDirectory::open(&root, SymlinkPolicy::Reject).unwrap();
    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("file"), "replacement").unwrap();

    let mut contents = String::new();
    scope
        .open_file(&root.join("file"))
        .unwrap()
        .read_to_string(&mut contents)
        .unwrap();
    assert_eq!(contents, "authorized");
    assert!(matches!(
        scope.open_file(&parent.path().join("file")),
        Err(ScopedReadError::OutsideRoot(_))
    ));
    assert!(matches!(
        scope.open_file(&root),
        Err(ScopedReadError::NotFile(_))
    ));
    assert!(matches!(
        scope.open_file(&root.join("missing")),
        Err(ScopedReadError::NotFound(_))
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn rejecting_symlinks_applies_to_the_root_and_every_component() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("nested/file"), "inside").unwrap();
    symlink("nested", root.path().join("link-dir")).unwrap();
    symlink("nested/file", root.path().join("link-file")).unwrap();
    let scope = ScopedDirectory::open(root.path(), SymlinkPolicy::Reject).unwrap();
    for name in ["link-file", "nested/../nested/file"] {
        assert!(matches!(
            scope.open_file(&root.path().join(name)),
            Err(ScopedReadError::OutsideRoot(_))
        ));
    }
    let directory_symlink_error = scope
        .open_file(&root.path().join("link-dir/file"))
        .unwrap_err();
    #[cfg(target_os = "linux")]
    assert!(matches!(
        directory_symlink_error,
        ScopedReadError::OutsideRoot(_)
    ));
    #[cfg(target_os = "macos")]
    assert!(matches!(
        directory_symlink_error,
        ScopedReadError::NotFile(_)
    ));
    assert!(ScopedDirectory::open(&root.path().join("link-dir"), SymlinkPolicy::Reject).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn within_root_policy_keeps_relative_symlinks_but_rejects_escapes() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "inside").unwrap();
    fs::write(outside.path().join("file"), "outside").unwrap();
    symlink("file", root.path().join("link")).unwrap();
    symlink(outside.path().join("file"), root.path().join("escape")).unwrap();
    assert_eq!(
        read_scoped_text_file(root.path(), &root.path().join("link")).unwrap(),
        "inside"
    );
    assert!(matches!(
        open_scoped_file(root.path(), &root.path().join("escape")),
        Err(ScopedReadError::OutsideRoot(_))
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn within_root_policy_rejects_replacement_root_symlinks() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("root");
    let outside = parent.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(root.join("file"), "inside").unwrap();
    fs::write(outside.join("file"), "outside").unwrap();
    assert_eq!(
        read_scoped_text_file(&root, &root.join("file")).unwrap(),
        "inside"
    );
    fs::rename(&root, parent.path().join("old-root")).unwrap();
    symlink(&outside, &root).unwrap();
    assert!(open_scoped_file(&root, &root.join("file")).is_err());
    for suffix in ["", "."] {
        let root = root.join(suffix);
        assert!(ScopedDirectory::open(&root, SymlinkPolicy::WithinRoot).is_err());
        assert!(ScopedDirectory::open(&root, SymlinkPolicy::Reject).is_err());
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn replacement_symlinks_never_change_the_opened_file() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    fs::write(&path, "inside").unwrap();
    fs::write(outside.path().join("file"), "outside").unwrap();
    let scope = ScopedDirectory::open(root.path(), SymlinkPolicy::Reject).unwrap();
    let mut file = scope.open_file(&path).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(outside.path().join("file"), &path).unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "inside");
    assert!(matches!(
        scope.open_file(&path),
        Err(ScopedReadError::OutsideRoot(_))
    ));
}
