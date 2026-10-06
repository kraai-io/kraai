use rmcp::model::ProtocolVersion;
use rmcp::service::{ClientInitializeError, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpClientTransportConfig,
};
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleClient};

use super::Handler;

pub(super) async fn connect(
    client: impl StreamableHttpClient,
    config: StreamableHttpClientTransportConfig,
    mut handler: Handler,
    lifecycle: ClientLifecycleMode,
) -> Result<RunningService<RoleClient, Handler>, String> {
    let transport = StreamableHttpClientTransport::with_client(client.clone(), config.clone());
    match handler
        .clone()
        .serve_with_lifecycle(transport, lifecycle)
        .await
    {
        Ok(connection) => Ok(connection),
        Err(error) => {
            let Some(version) = legacy_version(&error) else {
                return Err(error.to_string());
            };
            handler.protocol = version;
            let transport = StreamableHttpClientTransport::with_client(client, config);
            handler
                .serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
                .await
                .map_err(|error| format!("Legacy MCP initialization failed: {error}"))
        }
    }
}

fn legacy_version(error: &ClientInitializeError) -> Option<ProtocolVersion> {
    let ClientInitializeError::NoCompatibleProtocolVersion {
        server_supported, ..
    } = error
    else {
        return None;
    };
    ProtocolVersion::KNOWN_VERSIONS
        .iter()
        .rev()
        .find(|version| version.has_initialize() && server_supported.contains(version))
        .cloned()
}
