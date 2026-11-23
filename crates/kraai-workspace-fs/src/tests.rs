use std::fs;

use ulid::Ulid;

use super::*;

pub(super) fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kraai-workspace-fs-{name}-{}", Ulid::generate()));
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
            old_text: String::from("beta\n"),
            new_text: String::from("gamma\n"),
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
    assert!(matches!(
        error,
        WorkspaceFsError::Write { ref source, .. }
            if source.kind() == std::io::ErrorKind::AlreadyExists
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn overwrite_creates_or_replaces_and_keeps_permissions() {
    let directory = temp_dir("overwrite");
    let path = directory.join("file.txt");
    let (created, outcome) =
        overwrite_text_file(&directory, Path::new("file.txt"), "first\n").unwrap();
    assert_eq!(outcome, Overwrite::Created);
    assert_eq!(
        created.path,
        directory.canonicalize().unwrap().join("file.txt")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let permissions = path.metadata().unwrap().permissions();

    let (_, outcome) = overwrite_text_file(&directory, Path::new("file.txt"), "second\n").unwrap();
    assert_eq!(outcome, Overwrite::Replaced);
    assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");
    assert_eq!(path.metadata().unwrap().permissions(), permissions);

    fs::create_dir(directory.join("folder")).unwrap();
    let error = overwrite_text_file(&directory, Path::new("folder"), "x").unwrap_err();
    assert!(matches!(error, WorkspaceFsError::NotFile(_)));
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
            old_text: String::from("original\n"),
            new_text: String::from("replacement\n"),
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

#[test]
fn normalizes_missing_paths_without_requiring_the_file_to_exist() {
    assert_eq!(
        normalize_allow_missing(Path::new("/workspace/src"), Path::new("../missing.rs")),
        Path::new("/workspace/missing.rs")
    );
}
