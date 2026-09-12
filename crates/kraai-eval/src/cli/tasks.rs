use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, bail};
use kraai_eval::TaskManifest;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct Task {
    pub path: PathBuf,
    pub manifest: TaskManifest,
}

pub(super) fn default_directory() -> PathBuf {
    let local = PathBuf::from("evals/tasks");
    if local.is_dir() {
        local
    } else {
        std::env::var_os("KRAAI_EVAL_TASKS")
            .map(PathBuf::from)
            .unwrap_or(local)
    }
}

pub(super) fn discover(directory: &Path) -> Result<BTreeMap<String, Task>> {
    let entries = fs::read_dir(directory).wrap_err_with(|| {
        format!(
            "read task directory {}; use --tasks-dir to select another",
            directory.display()
        )
    })?;
    let mut tasks = BTreeMap::new();
    for entry in entries {
        let path = entry?.path();
        let manifest = if path.is_dir() {
            path.join("task.toml")
        } else {
            path
        };
        if manifest
            .extension()
            .is_none_or(|extension| extension != "toml")
            || !manifest.is_file()
        {
            continue;
        }
        let task = load(&manifest)?;
        let id = task.manifest.id.clone();
        if tasks.insert(id.clone(), task).is_some() {
            bail!("duplicate task id {id} in {}", directory.display());
        }
    }
    if tasks.is_empty() {
        bail!("no task manifests found in {}", directory.display());
    }
    Ok(tasks)
}

pub(super) fn select(directory: &Path, selectors: &[String]) -> Result<Vec<Task>> {
    if selectors.is_empty() {
        return Ok(discover(directory)?.into_values().collect());
    }
    let mut catalog = None;
    let mut ids = BTreeSet::new();
    let mut selected = Vec::new();
    for selector in selectors {
        let path = Path::new(selector);
        let task = if path.is_file() {
            load(path)?
        } else if path.join("task.toml").is_file() {
            load(&path.join("task.toml"))?
        } else {
            if catalog.is_none() {
                catalog = Some(discover(directory)?);
            }
            catalog
                .as_mut()
                .and_then(|tasks| tasks.remove(selector))
                .ok_or_else(|| {
                    color_eyre::eyre::eyre!(
                        "unknown or repeated task {selector:?}; use `kraai-eval list`"
                    )
                })?
        };
        if !ids.insert(task.manifest.id.clone()) {
            bail!("task {} was selected more than once", task.manifest.id);
        }
        selected.push(task);
    }
    Ok(selected)
}

fn load(path: &Path) -> Result<Task> {
    let path = path.canonicalize()?;
    let manifest = TaskManifest::load(&path)?;
    manifest.validate(path.parent().unwrap_or_else(|| Path::new(".")))?;
    Ok(Task { path, manifest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::ensure;

    #[test]
    fn selection_rejects_duplicate_task_ids_and_accepts_paths_without_a_catalog() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("kraai-eval-catalog-{}", ulid::Ulid::generate()));
        fs::create_dir_all(root.join("fixture"))?;
        fs::write(root.join("fixture/input"), "input")?;
        let path = root.join("task.toml");
        fs::write(
            &path,
            "schema_version = 1\nid = 'example'\nprompt = 'Repair the input.'\n[source]\ndirectory = 'fixture'\n[[grader.commands]]\ncommand = ['true']\n",
        )?;
        let selector = path.to_string_lossy().into_owned();
        let tasks = select(&root.join("missing"), std::slice::from_ref(&selector))?;
        ensure!(tasks.len() == 1);
        ensure!(select(&root, &[selector.clone(), selector.clone()]).is_err());
        fs::copy(&path, root.join("duplicate.toml"))?;
        ensure!(discover(&root).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
