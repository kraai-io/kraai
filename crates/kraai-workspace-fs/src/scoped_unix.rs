use std::fs::File;
use std::path::{Component, Path};

use rustix::fs::{Mode, OFlags, open, openat};

use crate::{ScopedReadError, validate_scoped_file};

pub fn open_scoped_file(root: &Path, path: &Path) -> Result<File, ScopedReadError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_error| ScopedReadError::OutsideRoot(path.to_path_buf()))?;
    let mut components = relative.components().peekable();
    if components.peek().is_none() {
        return Err(ScopedReadError::NotFile(path.to_path_buf()));
    }
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ScopedReadError::OutsideRoot(path.to_path_buf()));
    }
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW;
    let mut descriptor = open(root, flags | OFlags::DIRECTORY, Mode::empty()).map_err(|error| {
        ScopedReadError::OpenRoot {
            path: root.to_path_buf(),
            source: error.into(),
        }
    })?;
    while let Some(component) = components.next() {
        let flags = if components.peek().is_some() {
            flags | OFlags::DIRECTORY
        } else {
            flags
        };
        descriptor =
            openat(&descriptor, component.as_os_str(), flags, Mode::empty()).map_err(|error| {
                match error {
                    rustix::io::Errno::NOENT => ScopedReadError::NotFound(path.to_path_buf()),
                    rustix::io::Errno::LOOP => ScopedReadError::OutsideRoot(path.to_path_buf()),
                    rustix::io::Errno::NOTDIR => ScopedReadError::NotFile(path.to_path_buf()),
                    _ => ScopedReadError::Open {
                        path: path.to_path_buf(),
                        source: error.into(),
                    },
                }
            })?;
    }
    validate_scoped_file(File::from(descriptor), path)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "filesystem tests assert fixture operations"
)]
mod tests {
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::tests::temp_dir;

    #[test]
    fn scoped_walk_rejects_fifos_without_blocking() {
        crate::tests::assert_scoped_reads_reject_fifos(open_scoped_file);
    }

    #[test]
    fn scoped_walk_reads_nested_files_and_atomic_replacements() {
        let root = temp_dir("scoped-walk");
        fs::create_dir(root.join("nested")).unwrap();
        let path = root.join("nested/file.txt");
        for contents in ["original\r\nsecond line\n", "updated\n"] {
            let replacement = root.join("replacement");
            fs::write(&replacement, contents).unwrap();
            fs::rename(&replacement, &path).unwrap();
            let mut actual = String::new();
            open_scoped_file(&root, &path)
                .unwrap()
                .read_to_string(&mut actual)
                .unwrap();
            assert_eq!(actual, contents);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_walk_rejects_escapes_and_replaced_path_components() {
        let directory = temp_dir("scoped-walk-escape");
        let root = directory.join("workspace");
        let outside = directory.join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), "outside").unwrap();
        for path in [outside.join("secret"), root.join("../outside/secret")] {
            assert!(matches!(
                open_scoped_file(&root, &path),
                Err(ScopedReadError::OutsideRoot(_))
            ));
        }
        let file = root.join("file");
        fs::write(&file, "inside").unwrap();
        assert!(open_scoped_file(&root, &file).is_ok());
        fs::remove_file(&file).unwrap();
        symlink(outside.join("secret"), &file).unwrap();
        assert!(matches!(
            open_scoped_file(&root, &file),
            Err(ScopedReadError::OutsideRoot(_))
        ));
        let nested = root.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("secret"), "inside").unwrap();
        assert!(open_scoped_file(&root, &nested.join("secret")).is_ok());
        fs::remove_dir_all(&nested).unwrap();
        symlink(&outside, &nested).unwrap();
        assert!(open_scoped_file(&root, &nested.join("secret")).is_err());
        fs::remove_dir_all(&root).unwrap();
        symlink(&outside, &root).unwrap();
        assert!(open_scoped_file(&root, &root.join("secret")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn scoped_walk_rejects_non_files_and_reports_missing_paths() {
        let root = temp_dir("scoped-walk-errors");
        fs::create_dir(root.join("directory")).unwrap();
        for path in [&root, &root.join("directory")] {
            assert!(matches!(
                open_scoped_file(&root, path),
                Err(ScopedReadError::NotFile(_))
            ));
        }
        assert!(matches!(
            open_scoped_file(&root, &root.join("missing")),
            Err(ScopedReadError::NotFound(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
