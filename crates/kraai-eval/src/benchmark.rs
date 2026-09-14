use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::{Result, ensure};
use serde::Serialize;

use crate::ResolvedHarness;
use crate::command::run_trusted;

#[derive(Serialize)]
pub struct BenchmarkSpec {
    pub schema_version: u32,
    pub harness: String,
    pub runner_bundle: PathBuf,
    pub runner_path: PathBuf,
    pub runner_args: Vec<String>,
    pub model: String,
    pub proxy_command: Vec<String>,
    pub bundle_sha256: String,
}

pub fn prepare_benchmark_spec(
    harness: ResolvedHarness,
    proxy_command: Vec<String>,
    artifact_dir: &Path,
) -> Result<PathBuf> {
    let store_root = runner_store_root(&harness.program)?;
    fs::create_dir_all(artifact_dir)?;
    let artifact_dir = artifact_dir.canonicalize()?;
    let closure = run_trusted(
        &[
            String::from("nix-store"),
            String::from("--query"),
            String::from("--requisites"),
            store_root.to_string_lossy().into_owned(),
        ],
        &artifact_dir,
        Duration::from_secs(120),
    )?;
    ensure!(
        closure.success(),
        "failed to resolve runner's Nix closure: {}",
        String::from_utf8_lossy(&closure.stderr)
    );
    let mut roots = String::from_utf8(closure.stdout)?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    ensure!(!roots.is_empty(), "runner Nix closure is empty");
    for root in &roots {
        ensure!(
            runner_store_root(Path::new(root))? == Path::new(root),
            "invalid Nix closure root"
        );
    }
    let archive_roots = roots
        .iter()
        .map(|root| root.strip_prefix('/').unwrap_or(root))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        artifact_dir.join("closure.txt"),
        format!("{archive_roots}\n"),
    )?;
    let runner_bundle = artifact_dir.join("runner.tar");
    let archive = run_trusted(
        &[
            String::from("tar"),
            String::from("--create"),
            String::from("--file"),
            runner_bundle.to_string_lossy().into_owned(),
            String::from("--directory"),
            String::from("/"),
            String::from("--sort=name"),
            String::from("--mtime=@0"),
            String::from("--owner=0"),
            String::from("--group=0"),
            String::from("--numeric-owner"),
            String::from("--verbatim-files-from"),
            String::from("--files-from"),
            artifact_dir
                .join("closure.txt")
                .to_string_lossy()
                .into_owned(),
        ],
        &artifact_dir,
        Duration::from_secs(600),
    )?;
    ensure!(
        archive.success(),
        "failed to bundle runner's Nix closure: {}",
        String::from_utf8_lossy(&archive.stderr)
    );
    let spec = BenchmarkSpec {
        schema_version: 1,
        harness: harness.name,
        bundle_sha256: crate::cache::hash_file(&runner_bundle)?,
        runner_bundle,
        runner_path: harness.program,
        runner_args: harness.args,
        model: harness.model_label,
        proxy_command,
    };
    let path = artifact_dir.join("kraai-eval-spec.json");
    fs::write(&path, serde_json::to_vec_pretty(&spec)?)?;
    Ok(path)
}

pub fn runner_store_root(program: &Path) -> Result<PathBuf> {
    let suffix = program.strip_prefix("/nix/store").map_err(|error| color_eyre::eyre::eyre!(
        "public benchmarks require a Nix-packaged runner; use `nix build .#kraai` and --runner ./result/bin/kraai: {error}"
    ))?;
    let component = suffix
        .components()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("runner has no Nix store root"))?;
    ensure!(
        matches!(component, std::path::Component::Normal(_)),
        "invalid Nix store root"
    );
    ensure!(
        !suffix
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir)),
        "runner path contains parent traversal"
    );
    Ok(Path::new("/nix/store").join(component.as_os_str()))
}

pub fn docker_proxy_host() -> Result<String> {
    let response = run_trusted(
        &[
            String::from("docker"),
            String::from("network"),
            String::from("inspect"),
            String::from("bridge"),
        ],
        &std::env::current_dir()?,
        Duration::from_secs(30),
    )?;
    ensure!(
        response.success(),
        "cannot inspect the local Docker bridge; use a running Docker engine or specify --proxy-host with a reachable controller IP"
    );
    let value: serde_json::Value = serde_json::from_slice(&response.stdout)?;
    let configs = value
        .pointer("/0/IPAM/Config")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("Docker bridge has no gateway; specify --proxy-host")
        })?;
    let address = configs
        .iter()
        .filter_map(|value| value.get("Gateway"))
        .filter_map(serde_json::Value::as_str)
        .find_map(|value| value.parse::<std::net::IpAddr>().ok())
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("Docker bridge has no IP gateway; specify --proxy-host")
        })?;
    ensure!(
        !address.is_loopback() && !address.is_unspecified(),
        "Docker gateway is not reachable from a container; specify --proxy-host"
    );
    Ok(address.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_roots_require_an_actual_store_location() -> Result<()> {
        ensure!(runner_store_root(Path::new("/tmp/runner")).is_err());
        ensure!(runner_store_root(Path::new("/nix/store")).is_err());
        ensure!(runner_store_root(Path::new("/nix/store/../secret")).is_err());
        ensure!(
            runner_store_root(Path::new("/nix/store/abc-runner/bin/runner"))?
                == Path::new("/nix/store/abc-runner")
        );
        Ok(())
    }
}
