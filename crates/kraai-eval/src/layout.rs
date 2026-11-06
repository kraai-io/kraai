use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, ensure, eyre};

use crate::cache::path_segment;

const SHORT_VERSION: usize = 8;
const DATE: &str = "%Y-%m-%d";

#[derive(Debug, Clone)]
pub struct EvalRoot {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct HarnessDir {
    pub path: PathBuf,
    pub relative: PathBuf,
}

impl EvalRoot {
    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .wrap_err_with(|| format!("create eval directory {}", root.display()))?;
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub fn default_path() -> Result<PathBuf> {
        Ok(kraai_persistence::agent_state_root()?.join("eval"))
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn cache(&self) -> PathBuf {
        self.root.join(".cache")
    }

    pub fn staging(&self, harness: &HarnessDir) -> PathBuf {
        self.cache().join("staging").join(&harness.relative)
    }

    pub fn lock_path(&self, harness: &HarnessDir) -> PathBuf {
        let mut name = harness.relative.as_os_str().to_owned();
        name.push(".lock");
        self.cache().join("locks").join(name)
    }

    pub fn harness_dir(
        &self,
        benchmark: &str,
        model: &str,
        harness: &str,
        version: &str,
    ) -> Result<HarnessDir> {
        let short = version
            .get(..SHORT_VERSION)
            .filter(|short| short.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| eyre!("harness version must be a sha256 digest"))?;
        let suffix = format!("-{}-{short}", path_segment(harness, "harness"));
        let parent = PathBuf::from(benchmark).join(model);
        let existing = existing_harness_dir(&self.root.join(&parent), &suffix)?;
        let name =
            existing.unwrap_or_else(|| format!("{}{suffix}", chrono::Local::now().format(DATE)));
        let relative = parent.join(name);
        Ok(HarnessDir {
            path: self.root.join(&relative),
            relative,
        })
    }
}

pub fn model_dir(model: &str, reasoning: Option<&str>) -> String {
    let model = path_segment(model, "model");
    match reasoning {
        Some(reasoning) => format!("{model}-{}", path_segment(reasoning, "reasoning")),
        None => model,
    }
}

fn existing_harness_dir(parent: &Path, suffix: &str) -> Result<Option<String>> {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).wrap_err_with(|| format!("read {}", parent.display())),
    };
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let dated = name
            .strip_suffix(suffix)
            .is_some_and(|date| chrono::NaiveDate::parse_from_str(date, DATE).is_ok());
        if dated && entry.file_type()?.is_dir() {
            matches.push(name);
        }
    }
    matches.sort();
    ensure!(
        matches.len() <= 1,
        "several result folders match one harness version in {}: {}",
        parent.display(),
        matches.join(", ")
    );
    Ok(matches.pop())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_dirs_keep_their_first_run_date() -> Result<()> {
        let temporary =
            std::env::temp_dir().join(format!("kraai-eval-layout-{}", ulid::Ulid::generate()));
        let root = EvalRoot::open(&temporary)?;
        let version = "482a30610bea240b574690a4dfdd23575eb5914a3fa9b913b263f6ee61bb747c";
        let model = model_dir("stealth/crouton-alpha", Some("high"));
        ensure!(model == "stealth-crouton-alpha-high");
        let fresh = root.harness_dir("in-house", &model, "kraai", version)?;
        let today = chrono::Local::now().format(DATE).to_string();
        ensure!(
            fresh.relative
                == Path::new("in-house/stealth-crouton-alpha-high")
                    .join(format!("{today}-kraai-482a3061"))
        );
        let saved = root
            .path()
            .join("in-house/stealth-crouton-alpha-high/2026-01-02-kraai-482a3061");
        fs::create_dir_all(&saved)?;
        fs::create_dir_all(
            root.path()
                .join("in-house/stealth-crouton-alpha-high/notes-kraai-482a3061"),
        )?;
        ensure!(root.harness_dir("in-house", &model, "kraai", version)?.path == saved);
        let other = root.harness_dir("in-house", &model, "codex", version)?;
        ensure!(other.relative.ends_with(format!("{today}-codex-482a3061")));
        ensure!(
            root.harness_dir("in-house", &model, "kraai", "short")
                .is_err()
        );
        ensure!(
            root.lock_path(&fresh)
                == root
                    .path()
                    .join(".cache/locks/in-house/stealth-crouton-alpha-high")
                    .join(format!("{today}-kraai-482a3061.lock"))
        );
        fs::remove_dir_all(temporary)?;
        Ok(())
    }
}
