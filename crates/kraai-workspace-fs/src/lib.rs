#![deny(unsafe_code)]

mod atomic_write;
mod edits;
mod error;

pub use edits::{ExactTextEdit, apply_exact_edits};
pub use error::{ScopedReadError, WorkspaceFsError};

#[cfg(windows)]
mod windows;

#[cfg(not(windows))]
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use atomic_write::{WriteMode, atomic_write};

pub fn resolve_path(cwd: &Path, requested: &Path) -> PathBuf {
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        cwd.join(requested)
    }
}

pub fn normalize_allow_missing(cwd: &Path, requested: &Path) -> PathBuf {
    let absolute = resolve_path(cwd, requested);
    if let Ok(canonical) = canonicalize(cwd, &absolute) {
        return canonical;
    }

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn canonicalize(cwd: &Path, path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    return path.canonicalize().or_else(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            windows::canonicalize(cwd, path)
        } else {
            Err(error)
        }
    });
    #[cfg(not(windows))]
    {
        let _ = cwd;
        path.canonicalize()
    }
}

pub fn validate_text_file(cwd: &Path, requested: &Path) -> Result<PathBuf, WorkspaceFsError> {
    read_validated_text_file(cwd, requested).map(|(path, _)| path)
}

fn read_validated_text_file(
    cwd: &Path,
    requested: &Path,
) -> Result<(PathBuf, String), WorkspaceFsError> {
    let path = resolve_path(cwd, requested);
    let canonical = canonicalize(cwd, &path).map_err(|source| WorkspaceFsError::Canonicalize {
        path: path.clone(),
        source,
    })?;
    let metadata = canonical
        .metadata()
        .map_err(|source| WorkspaceFsError::Metadata {
            path: canonical.clone(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(WorkspaceFsError::NotFile(canonical));
    }
    let contents =
        read_regular_text_file(&canonical).map_err(|source| WorkspaceFsError::ReadText {
            path: canonical.clone(),
            source,
        })?;
    Ok((canonical, contents))
}

pub fn read_regular_text_file(path: &Path) -> std::io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

/// Read a path while guaranteeing that resolution remains beneath `root`.
///
/// This is intended for host-side refreshes of paths that were authorized in a
/// sandbox. Linux `openat2` performs resolution and opening atomically, so a
/// concurrent symlink replacement cannot escape the approved root.
pub fn read_scoped_text_file(root: &Path, path: &Path) -> Result<String, ScopedReadError> {
    let mut file = open_scoped_file(root, path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|source| ScopedReadError::ReadText {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(contents)
}

#[cfg(target_os = "linux")]
pub fn open_scoped_file(root: &Path, path: &Path) -> Result<File, ScopedReadError> {
    use rustix::fs::{Mode, OFlags, ResolveFlags, open, openat2};

    let relative = path
        .strip_prefix(root)
        .map_err(|_error| ScopedReadError::OutsideRoot(path.to_path_buf()))?;
    if relative.as_os_str().is_empty() {
        return Err(ScopedReadError::NotFile(path.to_path_buf()));
    }
    let root_directory = open(
        root,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .map_err(|error| ScopedReadError::OpenRoot {
        path: root.to_path_buf(),
        source: std::io::Error::from_raw_os_error(error.raw_os_error()),
    })?;
    let descriptor = openat2(
        &root_directory,
        relative,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(|error| match error {
        rustix::io::Errno::NOENT => ScopedReadError::NotFound(path.to_path_buf()),
        rustix::io::Errno::XDEV | rustix::io::Errno::LOOP => {
            ScopedReadError::OutsideRoot(path.to_path_buf())
        }
        _ => ScopedReadError::Open {
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        },
    })?;
    let file = File::from(descriptor);
    let metadata = file.metadata().map_err(|source| ScopedReadError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ScopedReadError::NotFile(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(windows)]
pub use windows::open_scoped_file;

#[cfg(not(any(target_os = "linux", windows)))]
pub fn open_scoped_file(_root: &Path, path: &Path) -> Result<File, ScopedReadError> {
    Err(ScopedReadError::UnsupportedPlatform(path.to_path_buf()))
}

pub fn create_text_file(
    cwd: &Path,
    requested: &Path,
    contents: &str,
) -> Result<PathBuf, WorkspaceFsError> {
    let requested = resolve_path(cwd, requested);
    let name = requested
        .file_name()
        .ok_or_else(|| WorkspaceFsError::MissingFileName(requested.clone()))?;
    let parent = requested
        .parent()
        .ok_or_else(|| WorkspaceFsError::MissingParent(requested.clone()))?;
    let parent = canonicalize(cwd, parent).map_err(|source| WorkspaceFsError::Canonicalize {
        path: parent.to_path_buf(),
        source,
    })?;
    if !parent.is_dir() {
        return Err(WorkspaceFsError::NotDirectory(parent));
    }
    let destination = parent.join(name);
    atomic_write(&destination, contents.as_bytes(), WriteMode::Create)?;
    Ok(destination)
}

pub fn edit_text_file(
    cwd: &Path,
    requested: &Path,
    edits: &[ExactTextEdit],
) -> Result<PathBuf, WorkspaceFsError> {
    if edits.is_empty() {
        return Err(WorkspaceFsError::NoEdits);
    }
    let (path, original) = read_validated_text_file(cwd, requested)?;
    let updated = apply_exact_edits(&path, &original, edits)?;
    let permissions = path
        .metadata()
        .map_err(|source| WorkspaceFsError::Metadata {
            path: path.clone(),
            source,
        })?
        .permissions();
    atomic_write(
        &path,
        updated.as_bytes(),
        WriteMode::Replace { permissions },
    )?;
    Ok(path)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "filesystem unit tests use direct fixture and output assertions"
)]
mod tests {
    use std::fs;

    use ulid::Ulid;

    use super::*;

    pub(super) fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("kraai-workspace-fs-{name}-{}", Ulid::generate()));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn edit_preserves_trailing_newline_and_permissions() {
        let directory = temp_dir("edit");
        let path = directory.join("file.txt");
        fs::write(&path, "alpha\nbeta\n").unwrap();
        let permissions = path.metadata().unwrap().permissions();
        edit_text_file(
            &directory,
            Path::new("file.txt"),
            &[ExactTextEdit {
                start_line: 2,
                end_line: 2,
                old_text: String::from("beta"),
                new_text: String::from("gamma"),
            }],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
        assert_eq!(path.metadata().unwrap().permissions(), permissions);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn create_never_replaces_an_existing_file() {
        let directory = temp_dir("create");
        let path = directory.join("file.txt");
        fs::write(&path, "original").unwrap();
        let error = create_text_file(&directory, Path::new("file.txt"), "replacement").unwrap_err();
        assert!(matches!(error, WorkspaceFsError::Write { .. }));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn create_and_edit_support_a_maximum_length_filename() {
        let directory = temp_dir("long-filename");
        let path = directory.join("x".repeat(255));
        fs::write(&path, "valid filename").unwrap();
        fs::remove_file(&path).unwrap();

        create_text_file(&directory, &path, "original\n").unwrap();
        edit_text_file(
            &directory,
            &path,
            &[ExactTextEdit {
                start_line: 1,
                end_line: 1,
                old_text: String::from("original"),
                new_text: String::from("replacement"),
            }],
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "replacement\n");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn regular_reads_preserve_text_and_io_errors() {
        let root = temp_dir("regular-read");
        let path = root.join("file.txt");
        for contents in ["", "alpha\nβeta\n", "last line"] {
            fs::write(&path, contents).unwrap();
            assert_eq!(read_regular_text_file(&path).unwrap(), contents);
            assert_eq!(
                read_validated_text_file(&root, Path::new("file.txt"))
                    .unwrap()
                    .1,
                contents
            );
        }
        fs::write(&path, [0xff, 0xfe]).unwrap();
        let actual = read_regular_text_file(&path).unwrap_err();
        let expected = fs::read_to_string(&path).unwrap_err();
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.to_string(), expected.to_string());
        assert!(matches!(
            validate_text_file(&root, Path::new("file.txt")),
            Err(WorkspaceFsError::ReadText { .. })
        ));
        fs::remove_file(&path).unwrap();
        let actual = read_regular_text_file(&path).unwrap_err();
        let expected = fs::read_to_string(&path).unwrap_err();
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.to_string(), expected.to_string());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn regular_reads_reject_replacement_fifos_without_waiting_for_a_writer() {
        let root = temp_dir("regular-fifo");
        let path = root.join("file.txt");
        fs::write(&path, "initial").unwrap();
        assert_eq!(read_regular_text_file(&path).unwrap(), "initial");
        fs::remove_file(&path).unwrap();
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRWXU).unwrap();
        let fifo = path.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            send.send(read_regular_text_file(&path)).unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(1));
        let reader_completed = if result.is_err() {
            let unblock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&fifo)
                .unwrap();
            let rescued = receive.recv_timeout(std::time::Duration::from_secs(1));
            drop(unblock);
            !matches!(rescued, Err(std::sync::mpsc::RecvTimeoutError::Timeout))
        } else {
            true
        };
        if reader_completed {
            reader.join().unwrap();
        }
        fs::remove_dir_all(root).unwrap();
        assert_eq!(
            result.unwrap().unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolves_opened_paths_without_volume_manager_access() {
        let root = temp_dir("windows-final-path").canonicalize().unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        let file = root.join("résolved.txt");
        fs::write(&file, "contents").unwrap();
        let resolved = windows::canonicalize(&root, &root.join("nested/../résolved.txt")).unwrap();
        assert_eq!(resolved, file.canonicalize().unwrap());
        assert_eq!(
            windows::canonicalize(&root.join("nested"), &file).unwrap(),
            resolved
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn scoped_reads_reject_traversal_and_alternate_streams() {
        let root = temp_dir("windows-scoped-read").canonicalize().unwrap();
        let path = root.join("file.txt");
        fs::write(&path, "contents").unwrap();
        assert_eq!(read_scoped_text_file(&root, &path).unwrap(), "contents");
        for relative in ["..\\secret.txt", "file.txt:secret", "file.txt::$DATA"] {
            assert!(matches!(
                read_scoped_text_file(&root, &root.join(relative)),
                Err(ScopedReadError::OutsideRoot(_))
            ));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scoped_reads_reject_replacement_fifos_without_waiting_for_a_writer() {
        for replace_root in [false, true] {
            let directory = temp_dir("scoped-fifo");
            let root = directory.join("workspace");
            fs::create_dir(&root).unwrap();
            let path = root.join("file.txt");
            fs::write(&path, "initial").unwrap();
            assert_eq!(read_scoped_text_file(&root, &path).unwrap(), "initial");
            let fifo = if replace_root {
                fs::remove_dir_all(&root).unwrap();
                root.clone()
            } else {
                fs::remove_file(&path).unwrap();
                path.clone()
            };
            nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                send.send(open_scoped_file(&root, &path)).unwrap();
            });
            let result = receive.recv_timeout(std::time::Duration::from_secs(1));
            let reader_completed = if result.is_err() {
                let unblock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&fifo)
                    .unwrap();
                let rescued = receive.recv_timeout(std::time::Duration::from_secs(1));
                drop(unblock);
                !matches!(rescued, Err(std::sync::mpsc::RecvTimeoutError::Timeout))
            } else {
                true
            };
            if reader_completed {
                reader.join().unwrap();
            }
            fs::remove_dir_all(directory).unwrap();
            assert!(result.is_ok(), "scoped open waited for a FIFO writer");
            if replace_root {
                assert!(matches!(
                    result.unwrap(),
                    Err(ScopedReadError::OpenRoot { .. })
                ));
            } else {
                assert!(matches!(result.unwrap(), Err(ScopedReadError::NotFile(_))));
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scoped_read_survives_atomic_replacement_but_rejects_escape_symlinks() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("scoped-read");
        let outside = temp_dir("scoped-read-outside");
        let path = root.join("file.txt");
        let outside_path = outside.join("secret.txt");
        fs::write(&path, "first").unwrap();
        fs::write(&outside_path, "outside").unwrap();
        assert_eq!(read_scoped_text_file(&root, &path).unwrap(), "first");

        atomic_write(
            &path,
            b"second",
            WriteMode::Replace {
                permissions: path.metadata().unwrap().permissions(),
            },
        )
        .unwrap();
        assert_eq!(read_scoped_text_file(&root, &path).unwrap(), "second");

        fs::remove_file(&path).unwrap();
        symlink(&outside_path, &path).unwrap();
        assert!(matches!(
            read_scoped_text_file(&root, &path),
            Err(ScopedReadError::OutsideRoot(_))
        ));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}
