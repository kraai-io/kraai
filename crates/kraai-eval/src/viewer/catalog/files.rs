use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use color_eyre::eyre::{Context, Result, ensure};
use serde::de::DeserializeOwned;

use super::Catalog;

pub(super) const JSON_LIMIT: u64 = 8 * 1024 * 1024;
pub(super) const LOG_LIMIT: u64 = 2 * 1024 * 1024;

fn checked_path(root: &Path, path: &Path) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(root)?.is_symlink(),
        "symlink cache directories are not served"
    );
    let relative = path
        .strip_prefix(root)
        .wrap_err("artifact is outside the result cache")?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "invalid artifact path"
        );
        current.push(component);
        ensure!(
            !fs::symlink_metadata(&current)?.is_symlink(),
            "symlink artifacts are not served"
        );
    }
    ensure!(path.metadata()?.is_file(), "artifact is not a regular file");
    Ok(())
}

pub(super) fn read_bounded(root: &Path, path: &Path, limit: u64) -> Result<(Vec<u8>, bool)> {
    checked_path(root, path)?;
    let file = File::open(path)?;
    ensure!(file.metadata()?.is_file(), "artifact is not a regular file");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 > limit;
    bytes.truncate(limit as usize);
    Ok((bytes, truncated))
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
        path.exists().then(|| self.json(path)).flatten()
    }

    pub(super) fn directories(&mut self, root: &Path, depth: usize) -> Vec<PathBuf> {
        const LIMIT: usize = 50_000;
        let mut directories = vec![root.to_path_buf()];
        if !root.exists() || root.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
            return Vec::new();
        }
        for _ in 0..depth {
            let mut next = Vec::new();
            for directory in directories {
                let entries = match fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        self.warning(&directory, error);
                        continue;
                    }
                };
                for entry in entries {
                    self.total_scanned_entries += 1;
                    if self.total_scanned_entries > LIMIT {
                        self.warning(root, "directory scan limit reached");
                        return Vec::new();
                    }
                    match entry {
                        Ok(entry) if entry.file_type().is_ok_and(|kind| kind.is_dir()) => {
                            next.push(entry.path());
                        }
                        Ok(_) => {}
                        Err(error) => self.warning(&directory, error),
                    }
                }
            }
            next.sort();
            directories = next;
        }
        directories
    }
}

pub(super) fn logs(root: &Path, directory: &Path) -> BTreeMap<String, PathBuf> {
    if !directory.starts_with(root)
        || !directory
            .canonicalize()
            .is_ok_and(|resolved| resolved == directory)
    {
        return BTreeMap::new();
    }
    let names = [
        "result.json",
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
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file())
            && checked_path(directory, &path).is_ok()
        {
            logs.insert(name.replace('/', "--"), path);
        }
    }
    logs
}
