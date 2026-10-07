use std::fs;

use color_eyre::eyre::{Result, ensure};
use serde_json::Value;

use super::{Catalog, Fixture};
use crate::viewer::catalog::files;

#[test]
fn logs_return_prefixes_and_json_rejects_truncation() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = fixture.0.join("log");
    fs::write(&path, b"abc")?;
    ensure!(files::read_bounded(&fixture.0, &path, 0)? == (Vec::new(), true));
    ensure!(files::read_bounded(&fixture.0, &path, 3)? == (b"abc".to_vec(), false));
    ensure!(files::read_bounded(&fixture.0, &path, 2)? == (b"ab".to_vec(), true));
    ensure!(files::read_bounded(&fixture.0, &path, u64::MAX)? == (b"abc".to_vec(), false));
    fs::File::create(&path)?.set_len(files::JSON_LIMIT + 1)?;
    let error = files::read_json::<Value>(&fixture.0, &path).err();
    ensure!(
        error
            .as_ref()
            .is_some_and(|error| error.to_string().contains("exceeds"))
    );
    Ok(())
}

#[test]
fn optional_json_distinguishes_missing_from_unreadable_artifacts() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut catalog = Catalog {
        root: fixture.0.clone(),
        ..Catalog::default()
    };
    ensure!(
        catalog
            .optional_json::<Value>(&fixture.0.join("missing"))
            .is_none()
    );
    ensure!(catalog.warnings.is_empty());
    fs::create_dir(fixture.0.join("directory"))?;
    ensure!(
        catalog
            .optional_json::<Value>(&fixture.0.join("directory"))
            .is_none()
    );
    ensure!(catalog.warnings.len() == 1);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn replaced_directory_symlinks_are_not_served() -> Result<()> {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new()?;
    let outside = Fixture::new()?;
    let directory = fixture.0.join("nested");
    fs::create_dir(&directory)?;
    let path = directory.join("runner.stdout.log");
    fs::write(&path, b"inside")?;
    fs::write(outside.0.join("runner.stdout.log"), b"outside")?;
    ensure!(files::logs(&fixture.0, &directory).len() == 1);
    fs::rename(&directory, fixture.0.join("old"))?;
    symlink(&outside.0, &directory)?;
    ensure!(files::read_bounded(&fixture.0, &path, 10).is_err());
    ensure!(files::logs(&fixture.0, &directory).is_empty());
    Ok(())
}
