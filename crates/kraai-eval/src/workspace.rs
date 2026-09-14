use std::fs;
use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::{Result, bail};

use crate::TaskManifest;
use crate::cache::hash_file;
use crate::command::run_trusted;
use crate::manifest::{SourceSpec, fixture_files, resolve_public_path, resolve_repository};

pub(crate) fn materialize_base(
    task: &TaskManifest,
    task_dir: &Path,
    destination: &Path,
) -> Result<()> {
    fs::create_dir_all(destination)?;
    match &task.source {
        SourceSpec::Git(source) => materialize_git(task_dir, source, destination)?,
        SourceSpec::Directory(source) => {
            let directory = resolve_public_path(task_dir, &source.directory)?;
            for path in fixture_files(&directory)? {
                let output = destination.join(path.strip_prefix(&directory)?);
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&path, &output)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if crate::manifest::fixture_executable(&path)? {
                        0o755
                    } else {
                        0o644
                    };
                    fs::set_permissions(&output, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }
    init_repository(destination)?;
    if let Some(patch) = task.source.public_patch() {
        let patch = resolve_public_path(task_dir, patch)?.canonicalize()?;
        checked(
            &[
                String::from("git"),
                String::from("apply"),
                patch.to_string_lossy().into_owned(),
            ],
            destination,
        )?;
        commit_all(destination, "public task fixture")?;
    }
    Ok(())
}

fn materialize_git(
    task_dir: &Path,
    source: &crate::manifest::GitSourceSpec,
    destination: &Path,
) -> Result<()> {
    let repository = resolve_repository(task_dir, &source.repository)?.canonicalize()?;
    let archive = destination.with_extension("tar");
    checked(
        &[
            String::from("git"),
            String::from("-C"),
            repository.to_string_lossy().into_owned(),
            String::from("archive"),
            String::from("--format=tar"),
            format!("--output={}", archive.display()),
            source.revision.clone(),
        ],
        task_dir,
    )?;
    checked(
        &[
            String::from("tar"),
            String::from("-xf"),
            archive.to_string_lossy().into_owned(),
            String::from("-C"),
            destination.to_string_lossy().into_owned(),
        ],
        task_dir,
    )?;
    fs::remove_file(archive)?;
    Ok(())
}

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    checked(
        &[
            String::from("cp"),
            String::from("-a"),
            format!("{}/.", source.display()),
            destination.to_string_lossy().into_owned(),
        ],
        source,
    )?;
    Ok(())
}

pub(crate) fn capture_submission(
    workspace: &Path,
    output: &Path,
    max_bytes: u64,
) -> Result<String> {
    checked(
        &[
            String::from("git"),
            String::from("add"),
            String::from("-A"),
            String::from("--"),
            String::from("."),
        ],
        workspace,
    )?;
    let outcome = run_trusted(
        &[
            String::from("git"),
            String::from("diff"),
            String::from("--cached"),
            String::from("--binary"),
            String::from("--no-ext-diff"),
        ],
        workspace,
        Duration::from_secs(30),
    )?;
    if !outcome.success() {
        bail!(
            "capture submission failed: {}",
            String::from_utf8_lossy(&outcome.stderr)
        );
    }
    if outcome.stdout.len() as u64 > max_bytes {
        bail!("submission exceeds configured limit of {max_bytes} bytes");
    }
    fs::write(output, outcome.stdout)?;
    hash_file(output)
}

pub(crate) fn replay_submission(base: &Path, destination: &Path, patch: &Path) -> Result<()> {
    copy_tree(base, destination)?;
    if fs::metadata(patch)?.len() == 0 {
        return Ok(());
    }
    checked(
        &[
            String::from("git"),
            String::from("apply"),
            String::from("--binary"),
            patch.canonicalize()?.to_string_lossy().into_owned(),
        ],
        destination,
    )
}

fn init_repository(path: &Path) -> Result<()> {
    checked(
        &[
            String::from("git"),
            String::from("init"),
            String::from("--quiet"),
        ],
        path,
    )?;
    checked(
        &[
            String::from("git"),
            String::from("config"),
            String::from("user.name"),
            String::from("kraai-eval"),
        ],
        path,
    )?;
    checked(
        &[
            String::from("git"),
            String::from("config"),
            String::from("user.email"),
            String::from("eval@invalid"),
        ],
        path,
    )?;
    commit_all(path, "evaluation base")
}

fn commit_all(path: &Path, message: &str) -> Result<()> {
    checked(
        &[
            String::from("git"),
            String::from("add"),
            String::from("-A"),
            String::from("--force"),
        ],
        path,
    )?;
    checked(
        &[
            String::from("git"),
            String::from("commit"),
            String::from("--quiet"),
            String::from("-m"),
            String::from(message),
        ],
        path,
    )
}

pub(crate) fn commit_fixture(path: &Path, message: &str) -> Result<()> {
    commit_all(path, message)
}

fn checked(command: &[String], cwd: &Path) -> Result<()> {
    let timeout = Duration::from_secs(60);
    let outcome = run_trusted(command, cwd, timeout)?;
    if outcome.timed_out {
        bail!(
            "command timed out after {} seconds ({})",
            timeout.as_secs(),
            command.join(" ")
        );
    }
    if outcome.output_limit_exceeded {
        bail!("command exceeded the output limit ({})", command.join(" "));
    }
    if !outcome.success() {
        bail!(
            "command failed ({}): {}",
            command.join(" "),
            String::from_utf8_lossy(&outcome.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use color_eyre::eyre::{Result, ensure};

    use super::{capture_submission, init_repository};

    #[test]
    fn supplied_ignored_files_remain_tracked_when_submissions_are_replayed() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "kraai-eval-ignored-fixture-{}",
            ulid::Ulid::generate()
        ));
        let base = root.join("base");
        fs::create_dir_all(&base)?;
        fs::write(base.join(".gitignore"), "input.txt\ntarget/\n")?;
        fs::write(base.join("input.txt"), "broken\n")?;
        init_repository(&base)?;
        let agent = root.join("agent");
        super::copy_tree(&base, &agent)?;
        fs::write(agent.join("input.txt"), "fixed\n")?;
        fs::create_dir(agent.join("target"))?;
        fs::write(agent.join("target/output"), "build output\n")?;
        let patch = root.join("submission.patch");
        capture_submission(&agent, &patch, 4096)?;
        let grading = root.join("grading");
        super::replay_submission(&base, &grading, &patch)?;
        ensure!(
            fs::read_to_string(grading.join("input.txt"))? == "fixed\n",
            "ignored fixture edit was lost"
        );
        ensure!(
            !grading.join("target/output").exists(),
            "ignored build output entered submission"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_sources_hash_executable_bits_and_copy_readonly_files_as_writable() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("kraai-eval-directory-{}", ulid::Ulid::generate()));
        fs::create_dir_all(root.join("fixture"))?;
        let input = root.join("fixture/input");
        fs::write(&input, "input\n")?;
        fs::set_permissions(&input, fs::Permissions::from_mode(0o444))?;
        let mut task: crate::TaskManifest = toml::from_str(
            "schema_version = 1\nid = 'directory'\nprompt = 'Repair input.'\n[source]\ndirectory = 'fixture'\n[[grader.commands]]\ncommand = ['true']\n",
        )?;
        task.validate(&root)?;
        let initial = task.public_digest(&root)?;
        let output = root.join("workspace");
        super::materialize_base(&task, &root, &output)?;
        ensure!(fs::metadata(output.join("input"))?.permissions().mode() & 0o200 != 0);
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644))?;
        ensure!(
            initial == task.public_digest(&root)?,
            "readonly store permissions changed task identity"
        );
        fs::set_permissions(&input, fs::Permissions::from_mode(0o755))?;
        ensure!(
            initial != task.public_digest(&root)?,
            "executable permission did not change task identity"
        );
        fs::write(&input, "changed\n")?;
        ensure!(initial != task.public_digest(&root)?);
        task.grader.hidden_patch = Some("fixture/input".into());
        ensure!(
            task.validate(&root).is_err(),
            "hidden material was allowed inside the fixture"
        );
        task.grader.hidden_patch = None;
        std::os::unix::fs::symlink(&input, root.join("fixture/link"))?;
        ensure!(
            task.validate(&root).is_err(),
            "fixture symlink was accepted"
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn capture_submission_excludes_ignored_build_artifacts() -> Result<()> {
        let workspace = std::env::temp_dir().join(format!(
            "kraai-eval-capture-submission-{}",
            ulid::Ulid::generate()
        ));
        fs::create_dir(&workspace)?;
        fs::write(workspace.join(".gitignore"), "target/\n")?;
        fs::write(workspace.join("tracked.txt"), "base\n")?;
        init_repository(&workspace)?;

        fs::create_dir(workspace.join("target"))?;
        fs::write(workspace.join("target/large.bin"), vec![0_u8; 8192])?;
        fs::write(workspace.join("new-source.txt"), "submission\n")?;
        let patch_path = workspace.join("submission.patch");

        capture_submission(&workspace, &patch_path, 4096)?;
        let patch = fs::read_to_string(&patch_path)?;
        ensure!(patch.contains("new-source.txt"));
        ensure!(!patch.contains("target/large.bin"));

        fs::remove_dir_all(workspace)?;
        Ok(())
    }
}
