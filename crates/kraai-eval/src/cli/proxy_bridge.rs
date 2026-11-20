use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use color_eyre::eyre::Result;
use kraai_eval::ProxyBridgeRequest;

#[derive(Debug, Args)]
pub(super) struct ProxyBridgeArgs {
    #[arg(long)]
    listen: SocketAddr,
    #[arg(long)]
    socket: PathBuf,
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

pub(super) fn execute(args: ProxyBridgeArgs) -> Result<ExitCode> {
    kraai_eval::run_proxy_bridge(ProxyBridgeRequest {
        listen: args.listen,
        socket: args.socket,
        command: args.command,
    })
}
