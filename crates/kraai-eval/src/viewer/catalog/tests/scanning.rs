use std::fs;

use serde_json::json;

use super::{Catalog, Fixture, NATIVE, files, native, set};

fn limited_catalog(fixture: &Fixture, remaining: usize) -> Catalog {
    Catalog {
        root: fixture.0.canonicalize().unwrap(),
        total_scanned_entries: files::DIRECTORY_SCAN_LIMIT - remaining,
        ..Catalog::default()
    }
}

#[test]
fn scan_limit_retains_completed_native_results() {
    let fixture = Fixture::new();
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})));
    let mut other = native(json!({}));
    set(&mut other, "experiment_id", json!("other"));
    fixture.write(
        "in-house/model/2026-01-01-codex-0123abcd/task/attempt-2/result.json",
        &other,
    );
    let mut catalog = limited_catalog(&fixture, 5);
    catalog.harness_folders();
    assert!(catalog.attempts.len() == 1);
    assert!(
        catalog
            .attempts
            .first()
            .is_some_and(|attempt| attempt.status == "passed" && attempt.task == "task")
    );
    assert!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
    assert!(catalog.scan_limit_reached);
    assert!(catalog.warnings.len() == 1);
    assert!(
        catalog
            .warnings
            .first()
            .is_some_and(|warning| warning.contains("directory scan limit reached"))
    );
}

#[test]
fn scan_limit_never_returns_intermediate_directories_as_attempts() {
    let fixture = Fixture::new();
    fixture.write(&format!("{NATIVE}/result.json"), &native(json!({})));
    let mut catalog = limited_catalog(&fixture, 2);
    catalog.harness_folders();
    assert!(catalog.attempts.is_empty());
    assert!(catalog.versions.is_empty());
    assert!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
    assert!(catalog.scan_limit_reached);
    assert!(catalog.warnings.len() == 1);
}

#[test]
fn scan_limit_is_shared_across_calls_and_warns_once() {
    let fixture = Fixture::new();
    for directory in ["first/leaf", "second/leaf", "third/leaf"] {
        fs::create_dir_all(fixture.0.join(directory)).unwrap();
    }
    let mut catalog = limited_catalog(&fixture, 1);
    let root = catalog.root.clone();
    assert!(catalog.directories(&root.join("first"), 1) == vec![root.join("first/leaf")]);
    assert!(catalog.warnings.is_empty());
    for directory in ["second", "third"] {
        assert!(catalog.directories(&root.join(directory), 1).is_empty());
        assert!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
        assert!(catalog.warnings.len() == 1);
    }
}

#[test]
fn scan_counts_non_directory_entries_towards_its_global_limit() {
    let fixture = Fixture::new();
    fixture.write("files/one.json", &json!({}));
    fixture.write("files/two.json", &json!({}));
    let mut catalog = limited_catalog(&fixture, 1);
    assert!(catalog.directories(&fixture.0.join("files"), 1).is_empty());
    assert!(catalog.total_scanned_entries == files::DIRECTORY_SCAN_LIMIT);
    assert!(catalog.scan_limit_reached);
    assert!(catalog.warnings.len() == 1);
}
