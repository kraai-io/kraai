use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;

use color_eyre::eyre::{Result, ensure};
use serde::Serialize;

use super::ModelProxyRequest;
use crate::KraaiProviderConfigRequest;

pub struct ProxyServiceRequest {
    pub model_proxy: ModelProxyRequest,
    pub state_dir: PathBuf,
    pub listen_address: SocketAddr,
    pub advertise_host: String,
    pub provider_config: Option<KraaiProviderConfigRequest>,
}

#[derive(Serialize)]
struct Ready {
    schema_version: u32,
    base_url: String,
    environment: BTreeMap<String, String>,
    provider_id: Option<String>,
    provider_config: Option<PathBuf>,
    agent_profiles: Option<PathBuf>,
}

pub fn serve_model_proxy(request: ProxyServiceRequest) -> Result<()> {
    let advertise_host = advertised_host(&request.advertise_host)?;
    fs::create_dir(&request.state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&request.state_dir, fs::Permissions::from_mode(0o700))?;
    }
    let state_dir = request.state_dir.canonicalize()?;
    let proxy = request
        .model_proxy
        .start_at(state_dir.join("proxy.events.jsonl"), request.listen_address)?;
    let mut identity = serde_json::to_value(proxy.record())?;
    if let Some(identity) = identity.as_object_mut() {
        identity.remove("credential_fingerprint");
    }
    fs::write(
        state_dir.join("identity.json"),
        serde_json::to_vec_pretty(&identity)?,
    )?;
    let base_url = reqwest::Url::parse(&format!(
        "http://{}:{}{}",
        advertise_host,
        proxy.address.port(),
        proxy.base_path
    ))?
    .to_string();
    let mut environment = proxy.environment();
    for value in environment.values_mut() {
        if *value == proxy.base_url() {
            *value = base_url.clone();
        }
    }
    let (provider_id, provider_config) = request
        .provider_config
        .as_ref()
        .map(|config| {
            let config = config.prepare()?;
            Ok::<_, color_eyre::Report>((
                Some(config.selected_provider_id().to_owned()),
                Some(config.materialize(&state_dir, &base_url)?),
            ))
        })
        .transpose()?
        .unwrap_or_default();
    let ready = Ready {
        schema_version: 1,
        base_url,
        environment,
        provider_id,
        agent_profiles: provider_config
            .as_ref()
            .and_then(|path| path.parent())
            .map(|directory| directory.join("agents.toml")),
        provider_config,
    };
    let temporary = state_dir.join("ready.tmp");
    fs::write(&temporary, serde_json::to_vec(&ready)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temporary, state_dir.join("ready.json"))?;
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink())?;
    let metrics = proxy.finish()?;
    fs::write(
        state_dir.join("metrics.json"),
        serde_json::to_vec_pretty(&metrics)?,
    )?;
    fs::remove_file(state_dir.join("ready.json"))?;
    Ok(())
}

fn advertised_host(host: &str) -> Result<String> {
    let address = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(address) = address.parse::<std::net::IpAddr>() {
        return Ok(match address {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        });
    }
    ensure!(
        !host.is_empty()
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')),
        "advertise-host must be a hostname or IP address without a port"
    );
    Ok(host.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_hosts_support_ipv6_and_reject_ports() -> Result<()> {
        ensure!(advertised_host("::1")? == "[::1]");
        ensure!(advertised_host("[::1]")? == "[::1]");
        ensure!(advertised_host("127.0.0.1")? == "127.0.0.1");
        ensure!(advertised_host("controller.test")? == "controller.test");
        for host in [
            "localhost:1000",
            "127.0.0.1:1000",
            "[::1]:1000",
            "",
            "host/path",
            "user@host",
            "[[::1]]",
        ] {
            ensure!(advertised_host(host).is_err());
        }
        Ok(())
    }
}
