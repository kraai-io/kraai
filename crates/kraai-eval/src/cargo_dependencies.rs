use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Context, Result, bail};

use crate::cache::hash_chunks;
use crate::command::run_trusted_with_clean_environment;
use crate::sandbox::RustEnvironment;

const FETCH_TIMEOUT: Duration = Duration::from_secs(600);
const CACHE_SCHEMA: &[u8] = b"kraai-eval-cargo-dependencies-v2";

pub(crate) struct CargoDependencies {
    pub home: PathBuf,
    pub key: String,
    pub reused: bool,
}

pub(crate) fn prepare(
    cache_root: &Path,
    workspace: &Path,
    task_sha256: &str,
    rust: &RustEnvironment,
) -> Result<CargoDependencies> {
    let manifest = workspace.join("Cargo.toml");
    let lockfile = workspace.join("Cargo.lock");
    if !manifest.is_file() || !lockfile.is_file() {
        bail!("Rust evaluation tasks must contain Cargo.toml and Cargo.lock");
    }
    let cargo = rust.cargo.canonicalize()?;
    let key = hash_chunks(&[
        CACHE_SCHEMA,
        task_sha256.as_bytes(),
        cargo.to_string_lossy().as_bytes(),
        &fs::read(&lockfile)?,
    ]);
    let dependencies_root = cache_root.join("dependencies");
    let final_dir = dependencies_root.join(&key);
    let final_home = final_dir.join("cargo-home");
    if is_complete(&final_dir, &key) {
        return Ok(CargoDependencies {
            home: final_home,
            key,
            reused: true,
        });
    }

    fs::create_dir_all(dependencies_root.join("tmp"))?;
    let staging = dependencies_root
        .join("tmp")
        .join(ulid::Ulid::generate().to_string());
    let staging_home = create_staging_home(&staging)?;
    let result = fetch(&cargo, &manifest, &staging_home, rust).and_then(|()| {
        fs::write(staging.join("complete"), format!("{key}\n"))?;
        match fs::rename(&staging, &final_dir) {
            Ok(()) => Ok(false),
            Err(_error) if is_complete(&final_dir, &key) => {
                fs::remove_dir_all(&staging)?;
                Ok(true)
            }
            Err(error) => Err(error).wrap_err("atomically commit Cargo dependency cache"),
        }
    });
    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    let reused = result?;

    Ok(CargoDependencies {
        home: final_home,
        key,
        reused,
    })
}

fn create_staging_home(staging: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir(staging)?;
    let home = staging.join("cargo-home");
    fs::create_dir(&home)?;
    Ok(home)
}

fn is_complete(directory: &Path, expected_key: &str) -> bool {
    directory.join("cargo-home").is_dir()
        && fs::read_to_string(directory.join("complete"))
            .is_ok_and(|contents| contents.trim() == expected_key)
}

fn fetch(cargo: &Path, manifest: &Path, cargo_home: &Path, rust: &RustEnvironment) -> Result<()> {
    let command = vec![
        cargo.to_string_lossy().into_owned(),
        String::from("fetch"),
        String::from("--locked"),
        String::from("--manifest-path"),
        manifest.to_string_lossy().into_owned(),
    ];
    let environment = BTreeMap::from([
        (
            String::from("CARGO_HOME"),
            cargo_home.to_string_lossy().into_owned(),
        ),
        (String::from("CARGO_NET_OFFLINE"), String::from("false")),
        (
            String::from("CARGO_NET_GIT_FETCH_WITH_CLI"),
            String::from("false"),
        ),
        (String::from("GIT_CONFIG_GLOBAL"), String::from("/dev/null")),
        (String::from("GIT_CONFIG_NOSYSTEM"), String::from("1")),
        (String::from("GIT_TERMINAL_PROMPT"), String::from("0")),
        (
            String::from("HOME"),
            cargo_home.to_string_lossy().into_owned(),
        ),
        (String::from("PATH"), trusted_program_path(cargo, rust)?),
    ]);
    let outcome =
        run_trusted_with_clean_environment(&command, cargo_home, FETCH_TIMEOUT, &environment)?;
    if outcome.timed_out {
        bail!(
            "cargo fetch timed out after {} seconds",
            FETCH_TIMEOUT.as_secs()
        );
    }
    if outcome.output_limit_exceeded {
        bail!("cargo fetch exceeded the output limit");
    }
    if !outcome.success() {
        bail!(
            "cargo fetch failed: {}",
            String::from_utf8_lossy(&outcome.stderr).trim()
        );
    }
    Ok(())
}

fn trusted_program_path(cargo: &Path, rust: &RustEnvironment) -> Result<String> {
    let directories = std::iter::once(cargo)
        .chain(rust.programs.iter().map(PathBuf::as_path))
        .filter_map(Path::parent)
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    std::env::join_paths(directories)
        .wrap_err("construct trusted Cargo fetch PATH")?
        .into_string()
        .map_err(|path| {
            color_eyre::eyre::eyre!(
                "trusted Cargo fetch PATH is not valid UTF-8: {}",
                path.to_string_lossy()
            )
        })
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
