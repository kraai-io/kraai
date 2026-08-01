use std::fs;
use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::{Result, bail};

use crate::TaskManifest;
use crate::cache::hash_file;
use crate::command::run_trusted;
use crate::manifest::{resolve_public_path, resolve_repository};

pub(crate) fn materialize_base(
    task: &TaskManifest,
    task_dir: &Path,
    destination: &Path,
) -> Result<()> {
    fs::create_dir_all(destination)?;
    let repository = resolve_repository(task_dir, &task.source.repository)?.canonicalize()?;
    let archive = destination.with_extension("tar");
    checked(
        &[
            String::from("git"),
            String::from("-C"),
            repository.to_string_lossy().into_owned(),
            String::from("archive"),
            String::from("--format=tar"),
            format!("--output={}", archive.display()),
            task.source.revision.clone(),
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
    init_repository(destination)?;
    if let Some(patch) = &task.source.public_patch {
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
        &[String::from("git"), String::from("add"), String::from("-A")],
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
