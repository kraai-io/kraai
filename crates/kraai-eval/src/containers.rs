use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::{Result, ensure};

use crate::command::run_trusted_with_environment;

pub fn benchmark_environment() -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    if std::env::var_os("DOCKER_HOST").is_none()
        && std::env::var_os("DOCKER_CONTEXT").is_none()
        && !Path::new("/var/run/docker.sock").exists()
        && let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
    {
        let socket = Path::new(&runtime).join("podman/podman.sock");
        if socket.exists() {
            environment.insert("DOCKER_HOST".into(), format!("unix://{}", socket.display()));
        }
    }
    environment
}

fn docker(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = vec![String::from("docker")];
    command.extend(args.iter().map(|arg| (*arg).to_owned()));
    let result = run_trusted_with_environment(
        &command,
        &std::env::current_dir()?,
        Duration::from_secs(60),
        &benchmark_environment(),
    )?;
    ensure!(
        result.success(),
        "container engine command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

pub fn docker_proxy_host() -> Result<String> {
    let version: serde_json::Value = serde_json::from_slice(&docker(&[
        "version",
        "--format",
        "{{json .Server.Components}}",
    ])?)?;
    let podman = version.as_array().is_some_and(|components| {
        components.iter().any(|component| {
            component.get("Name").and_then(serde_json::Value::as_str) == Some("Podman Engine")
        })
    });
    if podman {
        let output = docker(&[
            "run",
            "--rm",
            "docker.io/library/alpine:3.22",
            "getent",
            "hosts",
            "host.containers.internal",
        ])?;
        let text = String::from_utf8(output)?;
        return reachable_address(text.split_whitespace().next().unwrap_or_default());
    }
    let networks: serde_json::Value =
        serde_json::from_slice(&docker(&["network", "inspect", "bridge"])?)?;
    let gateway = networks
        .pointer("/0/IPAM/Config")
        .and_then(serde_json::Value::as_array)
        .and_then(|configs| {
            configs
                .iter()
                .find_map(|config| config.get("Gateway").and_then(serde_json::Value::as_str))
        })
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("Docker bridge has no gateway; specify --proxy-host")
        })?;
    reachable_address(gateway)
}

fn reachable_address(value: &str) -> Result<String> {
    let address: std::net::IpAddr = value.parse()?;
    ensure!(
        !address.is_loopback() && !address.is_unspecified(),
        "container gateway is not reachable; specify --proxy-host"
    );
    Ok(address.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_addresses_must_be_reachable() -> Result<()> {
        for invalid in ["", "127.0.0.1", "::1", "0.0.0.0", "::"] {
            ensure!(reachable_address(invalid).is_err());
        }
        ensure!(reachable_address("169.254.1.2")? == "169.254.1.2");
        Ok(())
    }
}
