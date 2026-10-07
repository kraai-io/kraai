use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OAuthConfig {
    pub client_id: Option<String>,
    pub client_secret_env: Option<String>,
    pub client_metadata_url: Option<String>,
    pub scopes: Vec<String>,
    pub redirect_port: u16,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub prompt_max_bytes: usize,
    pub servers: BTreeMap<String, ServerConfig>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            prompt_max_bytes: 16 * 1024,
            servers: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default = "startup_timeout")]
    pub startup_timeout_secs: u64,
    #[serde(default = "call_timeout")]
    pub call_timeout_secs: u64,
    pub transport: TransportConfig,
}

impl ServerConfig {
    pub fn new(transport: TransportConfig) -> Self {
        Self {
            enabled: enabled(),
            description: String::new(),
            startup_timeout_secs: startup_timeout(),
            call_timeout_secs: call_timeout(),
            transport,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        bearer_token_env: Option<String>,
        #[serde(default)]
        oauth: Option<OAuthConfig>,
    },
}

const fn enabled() -> bool {
    true
}
const fn startup_timeout() -> u64 {
    20
}
const fn call_timeout() -> u64 {
    60
}

impl McpConfig {
    pub async fn load(path: &Path) -> Result<Self, String> {
        let Some(contents) = kraai_io::fs::read_optional_text_async(path)
            .await
            .map_err(|error| format!("Unable to read {}: {error}", path.display()))?
        else {
            return Ok(Self::default());
        };
        let mut config: Self = toml::from_str(&contents)
            .map_err(|error| format!("Invalid MCP config {}: {error}", path.display()))?;
        for server in config.servers.values_mut() {
            if let TransportConfig::Stdio { cwd: Some(cwd), .. } = &mut server.transport
                && cwd.is_relative()
            {
                *cwd = path.parent().unwrap_or(Path::new(".")).join(&*cwd);
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, server) in &self.servers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            {
                return Err(format!("Invalid MCP server alias {name:?}"));
            }
            if server.startup_timeout_secs == 0 || server.call_timeout_secs == 0 {
                return Err(format!("MCP server {name}: timeouts must be positive"));
            }
            for seconds in [server.startup_timeout_secs, server.call_timeout_secs] {
                if std::time::Instant::now()
                    .checked_add(std::time::Duration::from_secs(seconds))
                    .is_none()
                {
                    return Err(format!("MCP server {name}: timeout is too large"));
                }
            }
            match &server.transport {
                TransportConfig::Stdio { command, .. } if command.trim().is_empty() => {
                    return Err(format!("MCP server {name}: command must not be empty"));
                }
                TransportConfig::Http {
                    url,
                    headers,
                    bearer_token_env,
                    oauth,
                } => {
                    crate::headers::parse(headers)
                        .map_err(|error| format!("MCP server {name}: {error}"))?;
                    if crate::headers::has_authorization(headers)
                        && (bearer_token_env.is_some() || oauth.is_some())
                    {
                        return Err(format!(
                            "MCP server {name}: Authorization header conflicts with OAuth or bearer_token_env"
                        ));
                    }
                    let parsed = url::Url::parse(url)
                        .map_err(|error| format!("MCP server {name}: {error}"))?;
                    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                        return Err(format!("MCP server {name}: expected an HTTP or HTTPS URL"));
                    }
                    if bearer_token_env
                        .as_ref()
                        .is_some_and(|name| name.trim().is_empty())
                    {
                        return Err(format!(
                            "MCP server {name}: bearer_token_env must not be empty"
                        ));
                    }
                    if let Some(oauth) = oauth {
                        if bearer_token_env.is_some() {
                            return Err(format!(
                                "MCP server {name}: choose OAuth or bearer_token_env"
                            ));
                        }
                        if oauth.client_secret_env.is_some() && oauth.client_id.is_none() {
                            return Err(format!(
                                "MCP server {name}: client_secret_env requires client_id"
                            ));
                        }
                        for value in [
                            &oauth.client_id,
                            &oauth.client_secret_env,
                            &oauth.client_metadata_url,
                        ]
                        .into_iter()
                        .flatten()
                        {
                            if value.trim().is_empty() {
                                return Err(format!(
                                    "MCP server {name}: OAuth options must not be empty"
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
