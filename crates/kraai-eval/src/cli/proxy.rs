use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use color_eyre::eyre::{Result, bail};
use kraai_eval::{KraaiProviderConfigRequest, ModelProxyRequest, ProxyServiceRequest};

#[derive(Debug, Args)]
pub(super) struct ProxyArgs {
    #[command(flatten)]
    pricing: super::accounting::PricingArgs,
    #[arg(long)]
    state_dir: PathBuf,
    #[arg(long, default_value_t = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))]
    listen: SocketAddr,
    #[arg(long, default_value = "127.0.0.1")]
    advertise_host: String,
    #[arg(long, default_value = "codex_subscription", value_parser = ["codex_subscription", "openai"])]
    proxy: String,
    #[arg(long, default_value = "OPENAI_API_KEY")]
    credential_env: String,
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u64).range(1..))]
    max_requests: u64,
    #[arg(long)]
    provider_config: Option<PathBuf>,
    #[arg(long, requires = "provider_config")]
    provider: Option<String>,
}

pub(super) fn execute(args: ProxyArgs) -> Result<ExitCode> {
    if args.proxy != "codex_subscription" && args.provider_config.is_some() {
        bail!("provider-config requires the codex_subscription proxy");
    }
    let model_proxy = if args.proxy == "openai" {
        ModelProxyRequest::openai(args.credential_env, args.max_requests)
    } else {
        ModelProxyRequest::codex_subscription(args.max_requests)
    };
    kraai_eval::serve_model_proxy(ProxyServiceRequest {
        model_proxy: model_proxy.with_pricing(args.pricing.options()?),
        state_dir: args.state_dir,
        listen_address: args.listen,
        advertise_host: args.advertise_host,
        provider_config: args
            .provider_config
            .map(|source| KraaiProviderConfigRequest::new(source, args.provider)),
    })?;
    Ok(ExitCode::SUCCESS)
}
