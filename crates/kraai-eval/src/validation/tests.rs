use std::fs;

use super::*;

#[test]
fn bundled_graders_reject_baselines_and_mutations_and_accept_references() {
    let root = crate::eval_assets_directory().join("tasks");
    for entry in fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path().join("task.toml");
        if !path.is_file() {
            continue;
        }
        let id = path.display();
        let prepared = PreparedValidation::new(&path).unwrap();
        let result = prepared.run(grade_trusted).unwrap();
        assert!(
            result.passed(),
            "grader validation failed for {id}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn bundled_graders_allow_module_refactors_and_ignore_cargo_test_discovery() {
    for id in ["event-stream", "dependency-waves"] {
        let path = crate::eval_assets_directory()
            .join("tasks")
            .join(id)
            .join("task.toml");
        let prepared = PreparedValidation::new(&path).unwrap();
        let reference = prepared
            .task
            .grader
            .reference_patch
            .as_deref()
            .expect("missing reference");
        let workspace = prepared.workspace("refactored", &[reference]).unwrap();
        fs::rename(
            workspace.join("src/lib.rs"),
            workspace.join("src/implementation.rs"),
        )
        .unwrap();
        fs::write(
            workspace.join("src/lib.rs"),
            "const _: &str = env!(\"CARGO_MANIFEST_DIR\");\n#[cfg(feature = \"implementation\")]\nmod implementation;\n#[cfg(feature = \"implementation\")]\npub use implementation::*;\n",
        ).unwrap();
        let manifest = workspace.join("Cargo.toml");
        fs::write(
            &manifest,
            format!(
                "{}\n[features]\ndefault = [\"implementation\"]\nimplementation = []\n",
                fs::read_to_string(&manifest)
                    .unwrap()
                    .replace("[package]", "[package]\nautotests = false")
            ),
        )
        .unwrap();
        assert!(
            grade_trusted(&prepared.task, &workspace)
                .unwrap()
                .iter()
                .all(CommandOutcome::success),
            "valid Cargo features, environment, or module refactor was rejected for {id}"
        );
        let baseline = prepared.workspace("disabled-tests", &[]).unwrap();
        let manifest = baseline.join("Cargo.toml");
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)
                .unwrap()
                .replace("[package]", "[package]\nautotests = false"),
        )
        .unwrap();
        assert!(
            !grade_trusted(&prepared.task, &baseline)
                .unwrap()
                .iter()
                .all(CommandOutcome::success),
            "disabling Cargo test discovery bypassed {id}"
        );
    }
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
