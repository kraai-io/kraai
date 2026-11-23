use std::fs;

use serde_json::Value;

use super::{Catalog, Fixture};
use crate::viewer::catalog::files;

#[test]
fn logs_return_prefixes_and_json_rejects_truncation() {
    let fixture = Fixture::new();
    let path = fixture.0.join("log");
    fs::write(&path, b"abc").unwrap();
    assert!(files::read_bounded(&fixture.0, &path, 0).unwrap() == (Vec::new(), true));
    assert!(files::read_bounded(&fixture.0, &path, 3).unwrap() == (b"abc".to_vec(), false));
    assert!(files::read_bounded(&fixture.0, &path, 2).unwrap() == (b"ab".to_vec(), true));
    assert!(files::read_bounded(&fixture.0, &path, u64::MAX).unwrap() == (b"abc".to_vec(), false));
    fs::File::create(&path)
        .unwrap()
        .set_len(files::JSON_LIMIT + 1)
        .unwrap();
    let error = files::read_json::<Value>(&fixture.0, &path).err();
    assert!(
        error
            .as_ref()
            .is_some_and(|error| error.to_string().contains("exceeds"))
    );
}

#[test]
fn optional_json_distinguishes_missing_from_unreadable_artifacts() {
    let fixture = Fixture::new();
    let mut catalog = Catalog {
        root: fixture.0.clone(),
        ..Catalog::default()
    };
    assert!(
        catalog
            .optional_json::<Value>(&fixture.0.join("missing"))
            .is_none()
    );
    assert!(catalog.warnings.is_empty());
    fs::create_dir(fixture.0.join("directory")).unwrap();
    assert!(
        catalog
            .optional_json::<Value>(&fixture.0.join("directory"))
            .is_none()
    );
    assert!(catalog.warnings.len() == 1);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn replaced_directory_symlinks_are_not_served() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = Fixture::new();
    let directory = fixture.0.join("nested");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("runner.stdout.log");
    fs::write(&path, b"inside").unwrap();
    fs::write(outside.0.join("runner.stdout.log"), b"outside").unwrap();
    assert!(files::logs(&fixture.0, &directory).len() == 1);
    fs::rename(&directory, fixture.0.join("old")).unwrap();
    symlink(&outside.0, &directory).unwrap();
    assert!(files::read_bounded(&fixture.0, &path, 10).is_err());
    assert!(files::logs(&fixture.0, &directory).is_empty());
}
