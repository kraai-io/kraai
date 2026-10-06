use std::collections::BTreeMap;
use std::path::Path;

use agent_client_protocol::{Result, schema::v1 as acp};
use kraai_mcp::{McpConfig, ServerConfig, TransportConfig};

use crate::error;

pub(crate) fn config(servers: Vec<acp::McpServer>, cwd: &Path) -> Result<McpConfig> {
    let mut config = McpConfig::default();
    for server in servers {
        let (name, transport) = match server {
            acp::McpServer::Stdio(server) => {
                if !server.command.is_absolute() {
                    return Err(error::invalid("MCP executable path must be absolute"));
                }
                let command = server
                    .command
                    .into_os_string()
                    .into_string()
                    .map_err(|_command| error::invalid("MCP executable path must be UTF-8"))?;
                if command.contains('\0') || server.args.iter().any(|arg| arg.contains('\0')) {
                    return Err(error::invalid(
                        "MCP executable and arguments must not contain NUL",
                    ));
                }
                let mut env = BTreeMap::new();
                for variable in server.env {
                    if variable.name.is_empty()
                        || variable.name.contains(['=', '\0'])
                        || variable.value.contains('\0')
                    {
                        return Err(error::invalid("Invalid MCP environment variable"));
                    }
                    if env.insert(variable.name, variable.value).is_some() {
                        return Err(error::invalid("Duplicate MCP environment variable"));
                    }
                }
                (
                    server.name,
                    TransportConfig::Stdio {
                        command,
                        args: server.args,
                        env,
                        cwd: Some(cwd.to_path_buf()),
                    },
                )
            }
            acp::McpServer::Http(server) => {
                let mut headers = BTreeMap::new();
                for header in server.headers {
                    if headers
                        .insert(header.name.to_ascii_lowercase(), header.value)
                        .is_some()
                    {
                        return Err(error::invalid("Duplicate MCP HTTP header"));
                    }
                }
                (
                    server.name,
                    TransportConfig::Http {
                        url: server.url,
                        headers,
                        bearer_token_env: None,
                        oauth: None,
                    },
                )
            }
            acp::McpServer::Sse(_) => {
                return Err(error::invalid("MCP SSE transport is not supported"));
            }
            _ => return Err(error::invalid("Unsupported MCP transport")),
        };
        let server = ServerConfig::new(transport);
        if config.servers.insert(name, server).is_some() {
            return Err(error::invalid("Duplicate MCP server name"));
        }
    }
    config.validate().map_err(error::invalid)?;
    Ok(config)
}
