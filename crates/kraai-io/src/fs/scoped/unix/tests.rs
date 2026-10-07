use std::fs;
use std::os::unix::fs::symlink;

use super::*;

#[test]
fn fallback_walk_rejects_symlinks_and_reads_nested_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = open_root(directory.path()).unwrap();
    fs::create_dir(directory.path().join("nested")).unwrap();
    fs::write(directory.path().join("nested/file"), b"inside").unwrap();
    assert!(open_file_walk(&root, Path::new("nested/file"), directory.path()).is_ok());
    assert!(matches!(
        open_file_walk(&root, Path::new("../outside"), directory.path()),
        Err(ScopedReadError::OutsideRoot(_))
    ));
    symlink("nested", directory.path().join("alias")).unwrap();
    assert!(open_file_walk(&root, Path::new("alias/file"), directory.path()).is_err());
    symlink("file", directory.path().join("nested/link")).unwrap();
    assert!(open_file_walk(&root, Path::new("nested/link"), directory.path()).is_err());
    nix::unistd::mkfifo(
        directory.path().join("fifo").as_path(),
        nix::sys::stat::Mode::S_IRWXU,
    )
    .unwrap();
    assert!(matches!(
        open_file_walk(&root, Path::new("fifo"), directory.path()),
        Err(ScopedReadError::NotFile(_))
    ));
}

#[cfg(target_os = "linux")]
mod linux {
    use std::cell::Cell;
    use std::io::Read;

    use rustix::io::Errno;

    use super::*;

    const UNAVAILABLE: [Errno; 3] = [Errno::NOSYS, Errno::PERM, Errno::INVAL];

    fn forced_open(
        root: &File,
        relative: &Path,
        policy: SymlinkPolicy,
        error: Errno,
    ) -> Result<File, ScopedReadError> {
        open_file_with_openat2(root, relative, relative, policy, |_| Err(error))
    }

    #[test]
    fn unavailable_openat2_reads_nested_files_without_following_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let root = open_root(directory.path()).unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/file"), b"inside").unwrap();
        symlink("nested", directory.path().join("alias")).unwrap();
        symlink("file", directory.path().join("nested/link")).unwrap();
        symlink("/etc/passwd", directory.path().join("escape")).unwrap();
        nix::unistd::mkfifo(
            directory.path().join("fifo").as_path(),
            nix::sys::stat::Mode::S_IRWXU,
        )
        .unwrap();

        for error in UNAVAILABLE {
            let mut contents = String::new();
            forced_open(
                &root,
                Path::new("nested/file"),
                SymlinkPolicy::Reject,
                error,
            )
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
            assert_eq!(contents, "inside");
            for name in [
                "alias/file",
                "nested/link",
                "escape",
                "nested/../nested/file",
            ] {
                assert!(forced_open(&root, Path::new(name), SymlinkPolicy::Reject, error).is_err());
            }
            for name in ["fifo", "nested"] {
                assert!(matches!(
                    forced_open(&root, Path::new(name), SymlinkPolicy::Reject, error),
                    Err(ScopedReadError::NotFile(_))
                ));
            }
            assert!(matches!(
                forced_open(&root, Path::new("missing"), SymlinkPolicy::Reject, error),
                Err(ScopedReadError::NotFound(_))
            ));
        }
    }

    #[test]
    fn unavailable_openat2_keeps_the_opened_root_after_path_replacement() {
        let parent = tempfile::tempdir().unwrap();
        let original = parent.path().join("root");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("file"), "authorized").unwrap();
        let root = open_root(&original).unwrap();
        fs::rename(&original, parent.path().join("moved")).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("file"), "replacement").unwrap();

        for error in UNAVAILABLE {
            let mut contents = String::new();
            forced_open(&root, Path::new("file"), SymlinkPolicy::Reject, error)
                .unwrap()
                .read_to_string(&mut contents)
                .unwrap();
            assert_eq!(contents, "authorized");
        }
    }

    #[test]
    fn unavailable_openat2_preserves_within_root_policy_failures() {
        let directory = tempfile::tempdir().unwrap();
        let root = open_root(directory.path()).unwrap();
        fs::write(directory.path().join("file"), "inside").unwrap();
        symlink("file", directory.path().join("link")).unwrap();

        for error in UNAVAILABLE {
            for name in ["file", "link"] {
                let result = forced_open(&root, Path::new(name), SymlinkPolicy::WithinRoot, error);
                assert!(matches!(
                    result,
                    Err(ScopedReadError::Open { source, .. })
                        if source.raw_os_error() == Some(error.raw_os_error())
                ));
            }
        }
    }

    #[test]
    fn openat2_path_and_resource_errors_are_not_retried() {
        let directory = tempfile::tempdir().unwrap();
        let root = open_root(directory.path()).unwrap();
        fs::write(directory.path().join("file"), "inside").unwrap();

        for error in [
            Errno::ACCESS,
            Errno::AGAIN,
            Errno::IO,
            Errno::BADF,
            Errno::NOTDIR,
            Errno::NAMETOOLONG,
            Errno::TOOBIG,
        ] {
            assert!(matches!(
                forced_open(&root, Path::new("file"), SymlinkPolicy::Reject, error),
                Err(ScopedReadError::Open { source, .. })
                    if source.raw_os_error() == Some(error.raw_os_error())
            ));
        }
        assert!(matches!(
            forced_open(
                &root,
                Path::new("file"),
                SymlinkPolicy::Reject,
                Errno::NOENT
            ),
            Err(ScopedReadError::NotFound(_))
        ));
        for error in [Errno::XDEV, Errno::LOOP] {
            assert!(matches!(
                forced_open(&root, Path::new("file"), SymlinkPolicy::Reject, error),
                Err(ScopedReadError::OutsideRoot(_))
            ));
        }
    }

    #[test]
    fn invalid_paths_are_rejected_before_the_syscall() {
        let directory = tempfile::tempdir().unwrap();
        let root = open_root(directory.path()).unwrap();

        for name in ["../outside", "nested/../file", "/absolute"] {
            let called = Cell::new(false);
            let result = open_file_with_openat2(
                &root,
                Path::new(name),
                Path::new(name),
                SymlinkPolicy::Reject,
                |_| {
                    called.set(true);
                    Err(Errno::NOSYS)
                },
            );
            assert!(matches!(result, Err(ScopedReadError::OutsideRoot(_))));
            assert!(!called.get());
        }
        for policy in [SymlinkPolicy::Reject, SymlinkPolicy::WithinRoot] {
            let called = Cell::new(false);
            let result = open_file_with_openat2(
                &root,
                Path::new("file\0suffix"),
                Path::new("file\0suffix"),
                policy,
                |_| {
                    called.set(true);
                    Err(Errno::NOSYS)
                },
            );
            assert!(matches!(
                result,
                Err(ScopedReadError::Open { source, .. })
                    if source.raw_os_error() == Some(Errno::INVAL.raw_os_error())
            ));
            assert!(!called.get());
        }
    }
}
