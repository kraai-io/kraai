use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, ensure};
use kraai_io::fs::{ScopedDirectory, ScopedReadError, SymlinkPolicy};
use kraai_io::read::read_prefix;
use serde::de::DeserializeOwned;

use super::Catalog;

pub(super) const JSON_LIMIT: u64 = 8 * 1024 * 1024;
pub(super) const LOG_LIMIT: u64 = 2 * 1024 * 1024;
pub(super) const DIRECTORY_SCAN_LIMIT: usize = 50_000;

pub(super) fn read_bounded(root: &Path, path: &Path, limit: u64) -> Result<(Vec<u8>, bool)> {
    let scope = ScopedDirectory::open(root, SymlinkPolicy::Reject)?;
    let file = scope.open_file(path)?;
    let prefix = read_prefix(file, limit)?;
    Ok((prefix.bytes, prefix.truncated))
}

pub(super) fn read_json<T: DeserializeOwned>(root: &Path, path: &Path) -> Result<T> {
    let (bytes, truncated) = read_bounded(root, path, JSON_LIMIT)?;
    ensure!(!truncated, "saved JSON exceeds the 8 MiB viewer limit");
    serde_json::from_slice(&bytes).wrap_err("invalid saved JSON")
}

impl Catalog {
    pub(super) fn json<T: DeserializeOwned>(&mut self, path: &Path) -> Option<T> {
        match read_json(&self.root, path) {
            Ok(value) => Some(value),
            Err(error) => {
                self.warning(path, format!("{error:#}"));
                None
            }
        }
    }

    pub(super) fn optional_json<T: DeserializeOwned>(&mut self, path: &Path) -> Option<T> {
        match read_json(&self.root, path) {
            Ok(value) => Some(value),
            Err(error)
                if matches!(
                    error.downcast_ref::<ScopedReadError>(),
                    Some(ScopedReadError::NotFound(_))
                ) =>
            {
                None
            }
            Err(error) => {
                self.warning(path, format!("{error:#}"));
                None
            }
        }
    }

    pub(super) fn directories(&mut self, root: &Path, depth: usize) -> Vec<PathBuf> {
        if self.scan_limit_reached
            || !root.exists()
            || root.symlink_metadata().is_ok_and(|meta| meta.is_symlink())
        {
            return Vec::new();
        }
        let mut directories = Vec::new();
        self.collect_directories(root, depth, &mut directories);
        directories.sort();
        directories
    }

    fn collect_directories(&mut self, root: &Path, depth: usize, directories: &mut Vec<PathBuf>) {
        if depth == 0 {
            directories.push(root.to_path_buf());
            return;
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                self.warning(root, error);
                return;
            }
        };
        for entry in entries {
            if self.total_scanned_entries == DIRECTORY_SCAN_LIMIT {
                self.scan_limit_reached = true;
                self.warning(
                    root,
                    "directory scan limit reached; showing discovered results only",
                );
                return;
            }
            self.total_scanned_entries += 1;
            match entry {
                Ok(entry) => match entry.file_type() {
                    Ok(kind) if kind.is_dir() => {
                        self.collect_directories(&entry.path(), depth - 1, directories);
                    }
                    Ok(_) => {}
                    Err(error) => self.warning(&entry.path(), error),
                },
                Err(error) => self.warning(root, error),
            }
            if self.scan_limit_reached {
                return;
            }
        }
    }
}

pub(super) fn logs(root: &Path, directory: &Path) -> BTreeMap<String, PathBuf> {
    let Ok(scope) = ScopedDirectory::open(root, SymlinkPolicy::Reject) else {
        return BTreeMap::new();
    };
    let names = [
        "result.json",
        "source.json",
        "events.jsonl",
        "proxy.events.jsonl",
        "harness-metrics.json",
        "request-accounting.json",
        "runner.stdout.log",
        "runner.stderr.log",
        "submission.patch",
        "trial.log",
        "exception.txt",
        "agent/runner.stdout.jsonl",
        "agent/runner.stderr.log",
        "agent/kraai-metrics.json",
        "agent/codex.txt",
        "agent/trajectory.json",
        "agent/trajectory-metrics.json",
        "verifier/test-stdout.txt",
        "verifier/test-stderr.txt",
        "verifier/reward.txt",
        "kraai-controller/proxy.events.jsonl",
        "kraai-controller/proxy-metrics.json",
        "kraai-controller/request-accounting.json",
        "kraai-controller/runner-metrics.json",
    ];
    let mut logs = BTreeMap::new();
    for name in names
        .into_iter()
        .map(str::to_owned)
        .chain((0..64).flat_map(|index| {
            [
                format!("grader-{index}.stdout.log"),
                format!("grader-{index}.stderr.log"),
            ]
        }))
    {
        let path = directory.join(&name);
        if scope.open_file(&path).is_ok() {
            logs.insert(name.replace('/', "--"), path);
        }
    }
    logs
}
