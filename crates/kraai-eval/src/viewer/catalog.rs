mod files;
mod harbor;
mod metrics;
mod native;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, ensure, eyre};
use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Catalog {
    pub versions: Vec<Version>,
    pub attempts: Vec<Attempt>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    prices: crate::Prices,
    #[serde(skip)]
    logs: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(skip)]
    sources: BTreeSet<PathBuf>,
    #[serde(skip)]
    task_revisions: BTreeMap<(String, String), String>,
    #[serde(skip)]
    total_scanned_entries: usize,
    #[serde(skip)]
    scan_limit_reached: bool,
}

#[derive(Debug, Serialize)]
pub struct Version {
    pub id: String,
    pub benchmark: String,
    pub harness: String,
    pub model: Option<String>,
    pub label: String,
    pub latest_at_ms: Option<u128>,
}

#[derive(Debug, Serialize)]
pub struct Attempt {
    pub id: String,
    pub version_id: String,
    pub task: String,
    pub attempt: u64,
    pub status: String,
    pub started_at_ms: Option<u128>,
    pub metrics: Metrics,
    pub error: Option<String>,
    pub logs: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
    pub input_tokens: Option<u128>,
    pub cached_input_tokens: Option<u128>,
    pub uncached_input_tokens: Option<u128>,
    pub output_tokens: Option<u128>,
    pub reasoning_tokens: Option<u128>,
    pub final_context_tokens: Option<u128>,
    pub turns: Option<u128>,
    pub requests: Option<u128>,
    pub duration_ms: Option<u128>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct LogContent {
    pub name: String,
    pub content: String,
    pub truncated: bool,
}

impl Catalog {
    pub fn load(root: &Path) -> Result<Self> {
        if !root.exists() {
            return Ok(Self {
                warnings: vec![format!("No saved results at {}", root.display())],
                ..Self::default()
            });
        }
        ensure!(
            root.is_dir(),
            "result cache is not a directory: {}",
            root.display()
        );
        let mut catalog = Self {
            root: root.canonicalize()?,
            ..Self::default()
        };
        match crate::Prices::load(&catalog.root) {
            Ok(prices) => catalog.prices = prices,
            Err(error) => {
                let root = catalog.root.clone();
                catalog.warning(&root, format!("{error:#}"));
            }
        }
        catalog.harness_folders();
        for version in &mut catalog.versions {
            version.latest_at_ms = catalog
                .attempts
                .iter()
                .filter(|attempt| attempt.version_id == version.id)
                .filter_map(|attempt| attempt.started_at_ms)
                .max();
        }
        catalog.versions.sort_by(|left, right| {
            right
                .latest_at_ms
                .cmp(&left.latest_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        catalog.attempts.sort_by(|left, right| {
            right
                .started_at_ms
                .cmp(&left.started_at_ms)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(catalog)
    }

    pub fn read_log(&self, attempt_id: &str, name: &str) -> Result<LogContent> {
        let path = self
            .logs
            .get(attempt_id)
            .and_then(|logs| logs.get(name))
            .ok_or_else(|| eyre!("unknown attempt log"))?;
        let (bytes, truncated) = files::read_bounded(&self.root, path, files::LOG_LIMIT)?;
        Ok(LogContent {
            name: name.to_owned(),
            content: String::from_utf8_lossy(&bytes).into_owned(),
            truncated,
        })
    }

    fn warning(&mut self, path: &Path, error: impl std::fmt::Display) {
        const LIMIT: usize = 100;
        if self.warnings.len() < LIMIT {
            let relative = path.strip_prefix(&self.root).unwrap_or(path);
            self.warnings.push(
                format!("{}: {error}", relative.display())
                    .chars()
                    .take(2048)
                    .collect(),
            );
        } else if self.warnings.len() == LIMIT {
            self.warnings.push(String::from("Further warnings omitted"));
        }
    }

    fn harness_folders(&mut self) {
        let root = self.root.clone();
        for directory in self.directories(&root, 3) {
            let parts = directory
                .strip_prefix(&root)
                .map(|relative| {
                    relative
                        .iter()
                        .map(|part| part.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let [benchmark, model, folder] = parts.as_slice() else {
                continue;
            };
            let (harness, label) = harness_and_label(folder);
            let id = self.source_id(&directory);
            self.versions.push(Version {
                id: id.clone(),
                benchmark: benchmark.clone(),
                harness,
                model: Some(model.clone()),
                label,
                latest_at_ms: None,
            });
            if directory.join("harbor").is_dir() {
                self.harbor_job(&directory.join("harbor"), &id);
            } else {
                self.native_folder(&directory, &id);
            }
        }
    }

    fn task_revision(&mut self, version_id: &str, task: &str, revision: String, path: &Path) {
        let key = (version_id.to_owned(), task.to_owned());
        if let Some(previous) = self.task_revisions.insert(key, revision.clone())
            && previous != revision
        {
            self.warning(
                path,
                format!("Task {task} has different saved task or grader revisions in this version"),
            );
        }
    }

    fn source_id(&self, path: &Path) -> String {
        crate::cache::hash_chunks(&[path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .as_os_str()
            .as_encoded_bytes()])
    }

    fn insert(&mut self, path: &Path, mut attempt: Attempt) {
        if !self.sources.insert(path.to_path_buf()) {
            return;
        }
        let logs = files::logs(&self.root, path);
        attempt.logs = logs.keys().cloned().collect();
        self.logs.insert(attempt.id.clone(), logs);
        self.attempts.push(attempt);
    }
}

fn harness_and_label(folder: &str) -> (String, String) {
    let parsed = folder
        .get(..10)
        .filter(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok())
        .and_then(|_| folder.get(11..))
        .and_then(|rest| rest.rsplit_once('-'))
        .filter(|(harness, label)| !harness.is_empty() && !label.is_empty());
    match parsed {
        Some((harness, hash)) => (harness.to_owned(), hash.to_owned()),
        None => (folder.to_owned(), folder.to_owned()),
    }
}

#[cfg(test)]
mod tests;
