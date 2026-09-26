use std::fs;

use color_eyre::eyre::{Result, ensure};
use serde_json::json;

use super::{Catalog, Fixture, NATIVE, files, native, set};

fn limited_catalog(fixture: &Fixture, remaining: usize) -> Result<Catalog> {
    Ok(Catalog {
        root: fixture.0.canonicalize()?,
        total_scanned_entries: files::DIRECTORY_SCAN_LIMIT - remaining,
        ..Catalog::default()
    })
}

#[test]
fn scan_limit_retains_completed_native_results() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})))?;
    let mut other = native(json!({}));
    set(&mut other, "experiment_id", json!("other"))?;
    fixture.write(
        "runs/task/codex/version/model/attempt-0/other/result.json",
        &other,
    )?;
    let mut catalog = limited_catalog(&fixture, 6)?;
    catalog.native_runs();
    ensure!(catalog.attempts.len() == 1);
    ensure!(
        catalog
            .attempts
            .first()
            .is_some_and(|attempt| attempt.status == "passed" && attempt.task == "task")
    );
    ensure!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
    ensure!(catalog.scan_limit_reached);
    ensure!(catalog.warnings.len() == 1);
    ensure!(
        catalog
            .warnings
            .first()
            .is_some_and(|warning| warning.contains("directory scan limit reached"))
    );
    Ok(())
}

#[test]
fn scan_limit_never_returns_intermediate_directories_as_attempts() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})))?;
    let mut catalog = limited_catalog(&fixture, 4)?;
    catalog.native_runs();
    ensure!(catalog.attempts.is_empty());
    ensure!(catalog.versions.is_empty());
    ensure!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
    ensure!(catalog.scan_limit_reached);
    ensure!(catalog.warnings.len() == 1);
    Ok(())
}

#[test]
fn scan_limit_is_shared_across_calls_and_warns_once() -> Result<()> {
    let fixture = Fixture::new()?;
    for directory in ["first/leaf", "second/leaf", "third/leaf"] {
        fs::create_dir_all(fixture.0.join(directory))?;
    }
    let mut catalog = limited_catalog(&fixture, 1)?;
    let root = catalog.root.clone();
    ensure!(catalog.directories(&root.join("first"), 1) == vec![root.join("first/leaf")]);
    ensure!(catalog.warnings.is_empty());
    for directory in ["second", "third"] {
        ensure!(catalog.directories(&root.join(directory), 1).is_empty());
        ensure!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
        ensure!(catalog.warnings.len() == 1);
    }
    Ok(())
}

#[test]
fn scan_counts_non_directory_entries_towards_its_global_limit() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.write("files/one.json", &json!({}))?;
    fixture.write("files/two.json", &json!({}))?;
    let mut catalog = limited_catalog(&fixture, 1)?;
    ensure!(catalog.directories(&fixture.0.join("files"), 1).is_empty());
    ensure!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
    ensure!(catalog.scan_limit_reached);
    ensure!(catalog.warnings.len() == 1);
    Ok(())
}
