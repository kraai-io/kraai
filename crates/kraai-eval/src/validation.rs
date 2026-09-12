use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::command::CommandOutcome;
use crate::manifest::resolve_private_path;
use crate::sandbox::{SandboxRequest, run_sandboxed, rust_environment};
use crate::workspace::{copy_tree, materialize_base};
use crate::{NetworkPolicy, TaskManifest};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskValidation {
    pub task_id: String,
    pub baseline_passed: bool,
    pub reference_passed: bool,
    pub mutations: Vec<MutationValidation>,
    pub diagnostics: Vec<String>,
}

impl TaskValidation {
    pub fn passed(&self) -> bool {
        !self.baseline_passed
            && self.reference_passed
            && self.mutations.iter().all(|mutation| !mutation.passed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationValidation {
    pub patch: PathBuf,
    pub passed: bool,
}

pub fn validate_task(path: &Path) -> Result<TaskValidation> {
    let prepared = PreparedValidation::new(path)?;
    let rust = prepared
        .task
        .runner
        .rust_toolchain
        .then(rust_environment)
        .transpose()?;
    let dependencies = rust
        .as_ref()
        .map(|rust| {
            crate::cargo_dependencies::prepare(
                &prepared.root.0,
                &prepared.base,
                &prepared.task.public_digest(&prepared.task_dir)?,
                rust,
            )
        })
        .transpose()?;
    prepared.run(|task, workspace| {
        let mut outcomes = Vec::new();
        for command in &task.grader.commands {
            outcomes.push(run_sandboxed(SandboxRequest {
                command: command.command.clone(),
                workspace: workspace.to_path_buf(),
                timeout: Duration::from_secs(command.timeout_seconds),
                network: NetworkPolicy::Disabled,
                environment: std::collections::BTreeMap::new(),
                extra_programs: rust
                    .as_ref()
                    .map(|rust| rust.programs.clone())
                    .unwrap_or_default(),
                cargo_home: dependencies
                    .as_ref()
                    .map(|dependencies| dependencies.home.clone()),
                metrics_output: None,
                script_executions_dir: None,
                resource_limits: Some(crate::resource_limits(task)),
            })?);
        }
        Ok(outcomes)
    })
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct PreparedValidation {
    task: TaskManifest,
    task_dir: PathBuf,
    root: TemporaryDirectory,
    base: PathBuf,
}

impl PreparedValidation {
    fn new(path: &Path) -> Result<Self> {
        let mut task = TaskManifest::load(path)?;
        let task_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()?;
        task.validate(&task_dir)?;
        task.resolve_source_revision(&task_dir)?;
        if task.grader.reference_patch.is_none() {
            bail!("task {} has no grader.reference_patch to validate", task.id);
        }
        let root = TemporaryDirectory(
            std::env::temp_dir().join(format!("kraai-eval-validation-{}", ulid::Ulid::generate())),
        );
        let base = root.0.join("base");
        materialize_base(&task, &task_dir, &base)?;
        Ok(Self {
            task,
            task_dir,
            root,
            base,
        })
    }

    fn workspace(&self, name: &str, patches: &[&Path]) -> Result<PathBuf> {
        let workspace = self.root.0.join(name);
        copy_tree(&self.base, &workspace)?;
        for patch in patches {
            crate::apply_patch(&workspace, &resolve_private_path(&self.task_dir, patch)?)?;
        }
        if let Some(patch) = &self.task.grader.hidden_patch {
            crate::apply_patch(&workspace, &resolve_private_path(&self.task_dir, patch)?)?;
        }
        Ok(workspace)
    }

    fn run(
        &self,
        mut grade: impl FnMut(&TaskManifest, &Path) -> Result<Vec<CommandOutcome>>,
    ) -> Result<TaskValidation> {
        let baseline = grade(&self.task, &self.workspace("baseline", &[])?)?;
        let reference_patch = self.task.grader.reference_patch.as_deref().ok_or_else(|| {
            color_eyre::eyre::eyre!("task {} has no reference patch", self.task.id)
        })?;
        let reference = grade(
            &self.task,
            &self.workspace("reference", &[reference_patch])?,
        )?;
        let mut mutations = Vec::new();
        for (index, patch) in self.task.grader.mutation_patches.iter().enumerate() {
            let outcomes = grade(
                &self.task,
                &self.workspace(&format!("mutation-{index}"), &[reference_patch, patch])?,
            )?;
            mutations.push(MutationValidation {
                patch: patch.clone(),
                passed: outcomes.iter().all(CommandOutcome::success),
            });
        }
        let baseline_passed = baseline.iter().all(CommandOutcome::success);
        let reference_passed = reference.iter().all(CommandOutcome::success);
        let mut diagnostics = Vec::new();
        if baseline_passed {
            diagnostics.push(String::from("unfinished baseline unexpectedly passed"));
        }
        for outcome in reference.iter().filter(|outcome| !outcome.success()) {
            diagnostics.push(format!(
                "reference failed: {}\n{}\n{}",
                outcome.command.join(" "),
                String::from_utf8_lossy(&outcome.stdout),
                String::from_utf8_lossy(&outcome.stderr),
            ));
        }
        for mutation in mutations.iter().filter(|mutation| mutation.passed) {
            diagnostics.push(format!(
                "incorrect mutation unexpectedly passed: {}",
                mutation.patch.display()
            ));
        }
        Ok(TaskValidation {
            task_id: self.task.id.clone(),
            baseline_passed,
            reference_passed,
            mutations,
            diagnostics,
        })
    }
}

#[cfg(test)]
mod tests {
    use color_eyre::eyre::ensure;

    use super::*;

    #[test]
    fn bundled_graders_reject_baselines_and_mutations_and_accept_references() -> Result<()> {
        let root = crate::eval_assets_directory().join("tasks");
        for id in ["event-stream", "dependency-waves"] {
            let prepared = PreparedValidation::new(&root.join(id).join("task.toml"))?;
            let result = prepared.run(grade_trusted)?;
            ensure!(
                result.passed(),
                "grader validation failed for {id}: {:?}",
                result.diagnostics
            );
        }
        Ok(())
    }

    #[test]
    fn bundled_graders_allow_module_refactors_and_ignore_cargo_test_discovery() -> Result<()> {
        for id in ["event-stream", "dependency-waves"] {
            let path = crate::eval_assets_directory()
                .join("tasks")
                .join(id)
                .join("task.toml");
            let prepared = PreparedValidation::new(&path)?;
            let reference = prepared
                .task
                .grader
                .reference_patch
                .as_deref()
                .ok_or_else(|| color_eyre::eyre::eyre!("missing reference"))?;
            let workspace = prepared.workspace("refactored", &[reference])?;
            fs::rename(
                workspace.join("src/lib.rs"),
                workspace.join("src/implementation.rs"),
            )?;
            fs::write(
                workspace.join("src/lib.rs"),
                "const _: &str = env!(\"CARGO_MANIFEST_DIR\");\n#[cfg(feature = \"implementation\")]\nmod implementation;\n#[cfg(feature = \"implementation\")]\npub use implementation::*;\n",
            )?;
            let manifest = workspace.join("Cargo.toml");
            fs::write(
                &manifest,
                format!(
                    "{}\n[features]\ndefault = [\"implementation\"]\nimplementation = []\n",
                    fs::read_to_string(&manifest)?
                        .replace("[package]", "[package]\nautotests = false")
                ),
            )?;
            ensure!(
                grade_trusted(&prepared.task, &workspace)?
                    .iter()
                    .all(CommandOutcome::success),
                "valid Cargo features, environment, or module refactor was rejected for {id}"
            );
            let baseline = prepared.workspace("disabled-tests", &[])?;
            let manifest = baseline.join("Cargo.toml");
            fs::write(
                &manifest,
                fs::read_to_string(&manifest)?.replace("[package]", "[package]\nautotests = false"),
            )?;
            ensure!(
                !grade_trusted(&prepared.task, &baseline)?
                    .iter()
                    .all(CommandOutcome::success),
                "disabling Cargo test discovery bypassed {id}"
            );
        }
        Ok(())
    }

    fn grade_trusted(task: &TaskManifest, workspace: &Path) -> Result<Vec<CommandOutcome>> {
        task.grader
            .commands
            .iter()
            .map(|command| {
                crate::command::run_trusted(
                    &command.command,
                    workspace,
                    Duration::from_secs(command.timeout_seconds),
                )
            })
            .collect()
    }
}
