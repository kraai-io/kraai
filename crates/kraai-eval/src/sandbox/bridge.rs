use std::ffi::OsString;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use color_eyre::eyre::{Context, Result};

use crate::proxy::ProxyListen;
use crate::temporary_directory::TemporaryDirectory;

pub const PROXY_BRIDGE_COMMAND: &str = "sandbox-proxy-bridge";
const SANDBOX_PROXY_ADDRESS: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 39_217));
#[cfg(target_os = "linux")]
const SANDBOX_PROGRAM: &str = "/run/kraai-eval/proxy-bridge";
#[cfg(target_os = "linux")]
const SANDBOX_SOCKET_DIR: &str = "/run/kraai-eval/model-proxy";
const SOCKET_NAME: &str = "proxy.sock";
#[cfg(unix)]
const MAX_CONNECTIONS: usize = 32;

pub(crate) struct ProxyBridge {
    program: PathBuf,
    socket_dir: TemporaryDirectory,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), expect(dead_code))]
pub(crate) struct BridgeMount {
    pub(super) program: PathBuf,
    socket_dir: PathBuf,
}

impl ProxyBridge {
    pub(crate) fn create(program: &Path) -> Result<Self> {
        let program = program
            .canonicalize()
            .wrap_err_with(|| format!("resolve model proxy bridge {}", program.display()))?;
        let path =
            std::env::temp_dir().join(format!("kraai-eval-proxy-{}", ulid::Ulid::generate()));
        fs::create_dir(&path)?;
        let socket_dir = TemporaryDirectory::new(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(socket_dir.path(), fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            program,
            socket_dir,
        })
    }

    pub(crate) fn proxy_listen(&self) -> ProxyListen {
        ProxyListen::Unix {
            path: self.socket_dir.path().join(SOCKET_NAME),
            advertised: SANDBOX_PROXY_ADDRESS,
        }
    }

    pub(crate) fn mount(&self) -> BridgeMount {
        BridgeMount {
            program: self.program.clone(),
            socket_dir: self.socket_dir.path().to_path_buf(),
        }
    }
}

#[cfg(target_os = "linux")]
impl BridgeMount {
    pub(super) fn bind_args(&self) -> Result<Vec<String>> {
        Ok(vec![
            String::from("--ro-bind"),
            self.program.to_string_lossy().into_owned(),
            String::from(SANDBOX_PROGRAM),
            String::from("--ro-bind"),
            self.socket_dir
                .canonicalize()?
                .to_string_lossy()
                .into_owned(),
            String::from(SANDBOX_SOCKET_DIR),
        ])
    }

    pub(super) fn command_prefix() -> Vec<String> {
        vec![
            String::from(SANDBOX_PROGRAM),
            String::from(PROXY_BRIDGE_COMMAND),
            String::from("--listen"),
            SANDBOX_PROXY_ADDRESS.to_string(),
            String::from("--socket"),
            format!("{SANDBOX_SOCKET_DIR}/{SOCKET_NAME}"),
            String::from("--"),
        ]
    }
}

pub struct ProxyBridgeRequest {
    pub listen: SocketAddr,
    pub socket: PathBuf,
    pub command: Vec<OsString>,
}

pub fn run_proxy_bridge(request: ProxyBridgeRequest) -> Result<ExitCode> {
    #[cfg(not(unix))]
    {
        let _ = request;
        color_eyre::eyre::bail!("the model proxy bridge needs a Unix host");
    }
    #[cfg(unix)]
    {
        let status = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(forwarding::run(request))?;
        Ok(forwarding::exit_code(status))
    }
}

#[cfg(unix)]
mod forwarding {
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{ExitCode, ExitStatus};
    use std::sync::Arc;

    use color_eyre::eyre::{Context, Result};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream, UnixStream};
    use tokio::sync::Semaphore;

    use super::{MAX_CONNECTIONS, ProxyBridgeRequest};

    const ACCEPT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

    pub(super) async fn run(request: ProxyBridgeRequest) -> Result<ExitStatus> {
        let (program, args) = request
            .command
            .split_first()
            .ok_or_else(|| color_eyre::eyre::eyre!("model proxy bridge command is empty"))?;
        let listener = TcpListener::bind(request.listen)
            .await
            .wrap_err_with(|| format!("listen for model proxy clients on {}", request.listen))?;
        let forwarder = tokio::spawn(forward_connections(listener, request.socket));
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        let status = kraai_sandbox::spawn_tokio_command(&mut command)
            .wrap_err_with(|| format!("spawn {}", program.to_string_lossy()))?
            .wait()
            .await;
        forwarder.abort();
        Ok(status?)
    }

    pub(super) async fn forward_connections(listener: TcpListener, socket: PathBuf) {
        let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        loop {
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                return;
            };
            let Ok((client, _)) = listener.accept().await else {
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                continue;
            };
            let socket = socket.clone();
            tokio::spawn(async move {
                let _ = forward_connection(client, &socket).await;
                drop(permit);
            });
        }
    }

    /// Relays one connection. It ends once the proxy closes its side, even when the client
    /// never closes its own, so a lingering client cannot hold a connection slot.
    pub(super) async fn forward_connection(mut client: TcpStream, socket: &Path) -> io::Result<()> {
        client.set_nodelay(true)?;
        let mut upstream = UnixStream::connect(socket).await?;
        let (mut client_read, mut client_write) = client.split();
        let (mut upstream_read, mut upstream_write) = upstream.split();
        let request = async {
            tokio::io::copy(&mut client_read, &mut upstream_write).await?;
            upstream_write.shutdown().await
        };
        let response = async {
            tokio::io::copy(&mut upstream_read, &mut client_write).await?;
            client_write.shutdown().await
        };
        tokio::pin!(request, response);
        tokio::select! {
            result = &mut response => result,
            result = &mut request => {
                result?;
                response.await
            }
        }
    }

    pub(super) fn exit_code(status: ExitStatus) -> ExitCode {
        let code = status
            .code()
            .or_else(|| status.signal().map(|signal| 128 + signal))
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(1);
        ExitCode::from(code)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitCode, ExitStatus};

    use color_eyre::eyre::{Result, ensure};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream, UnixListener};

    use super::forwarding::{exit_code, forward_connection, forward_connections};

    #[tokio::test]
    async fn forwards_loopback_clients_to_the_proxy_socket() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("kraai-eval-bridge-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root)?;
        let socket = root.join("proxy.sock");
        let upstream = UnixListener::bind(&socket)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let forwarder = tokio::spawn(forward_connections(listener, socket));
        let exchange = async {
            let upstream = async {
                let (mut stream, _) = upstream.accept().await?;
                let mut request = Vec::new();
                stream.read_to_end(&mut request).await?;
                stream.write_all(b"pong:").await?;
                stream.write_all(&request).await?;
                stream.shutdown().await?;
                Ok::<_, color_eyre::Report>(())
            };
            let client = async {
                let mut stream = TcpStream::connect(address).await?;
                stream.write_all(b"ping").await?;
                stream.shutdown().await?;
                let mut response = Vec::new();
                stream.read_to_end(&mut response).await?;
                Ok::<_, color_eyre::Report>(response)
            };
            let (upstream, client) = tokio::join!(upstream, client);
            upstream?;
            client
        };
        let response = kraai_test_support::hang_guard(exchange).await??;
        forwarder.abort();
        std::fs::remove_dir_all(root)?;
        ensure!(
            response == b"pong:ping",
            "unexpected bridged response {response:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn relay_ends_when_the_proxy_closes_on_a_lingering_client() -> Result<()> {
        let root =
            std::env::temp_dir().join(format!("kraai-eval-bridge-{}", ulid::Ulid::generate()));
        std::fs::create_dir(&root)?;
        let socket = root.join("proxy.sock");
        let upstream = UnixListener::bind(&socket)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let mut client = TcpStream::connect(listener.local_addr()?).await?;
        let (accepted, _) = listener.accept().await?;
        let exchange = async {
            let proxy = async {
                let (mut stream, _) = upstream.accept().await?;
                stream.write_all(b"done").await?;
                Ok::<_, color_eyre::Report>(())
            };
            let (relay, proxy) = tokio::join!(forward_connection(accepted, &socket), proxy);
            relay?;
            proxy?;
            let mut response = Vec::new();
            client.read_to_end(&mut response).await?;
            Ok::<_, color_eyre::Report>(response)
        };
        let response = kraai_test_support::hang_guard(exchange).await??;
        std::fs::remove_dir_all(root)?;
        ensure!(
            response == b"done",
            "unexpected bridged response {response:?}"
        );
        Ok(())
    }

    #[test]
    fn exit_status_survives_the_bridge() {
        assert_eq!(
            exit_code(ExitStatus::from_raw(7 << 8)),
            ExitCode::from(7),
            "exit codes must pass through"
        );
        assert_eq!(
            exit_code(ExitStatus::from_raw(9)),
            ExitCode::from(137),
            "signals must map to 128 + signal"
        );
    }
}
