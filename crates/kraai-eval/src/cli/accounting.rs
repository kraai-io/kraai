use std::path::{Path, PathBuf};

use clap::Args;
use color_eyre::eyre::{Result, ensure};
use kraai_eval::{AccountingSummary, PricingOptions, SuiteResult};

#[derive(Debug, Clone, Default, Args)]
pub(super) struct PricingArgs {
    #[arg(
        long,
        help = "Provider configuration containing benchmark price overrides; model IDs must match actual request models"
    )]
    pricing_config: Option<PathBuf>,
    #[arg(
        long,
        requires = "pricing_config",
        help = "Provider ID to use for price estimates"
    )]
    pricing_provider: Option<String>,
}

impl PricingArgs {
    pub fn options(&self) -> Result<PricingOptions> {
        let options = PricingOptions::new(
            self.pricing_config
                .as_ref()
                .map(|path| path.canonicalize())
                .transpose()?,
            self.pricing_provider.clone(),
        );
        options.validate()?;
        Ok(options)
    }

    pub fn append(&self, command: &mut Vec<String>) -> Result<()> {
        if let Some(path) = self.options()?.config {
            command.extend([
                String::from("--pricing-config"),
                path.to_string_lossy().into_owned(),
            ]);
        }
        if let Some(provider) = &self.pricing_provider {
            command.extend([String::from("--pricing-provider"), provider.clone()]);
        }
        Ok(())
    }
}

#[derive(Debug, Args)]
pub(super) struct AccountingArgs {
    #[arg(help = "Saved native result.json, suite summary.json, or Harbor job directory")]
    path: PathBuf,
    #[command(flatten)]
    pricing: PricingArgs,
}

pub(super) fn execute(args: AccountingArgs, json: bool) -> Result<()> {
    let options = args.pricing.options()?;
    let mut outputs = Vec::new();
    for (events, expected) in request_logs(&args.path)? {
        let accounting = kraai_eval::analyze_requests(&events, expected, &options)?;
        let output = events.with_file_name("request-accounting.json");
        std::fs::write(&output, serde_json::to_vec_pretty(&accounting)?)?;
        if !json {
            println!(
                "{}\n{}",
                output.display(),
                format_accounting(&accounting.summary())
            );
        }
        outputs.push(serde_json::json!({"path": output, "accounting": accounting}));
    }
    ensure!(
        !outputs.is_empty(),
        "no saved proxy request logs were found"
    );
    if json {
        super::print_json(&outputs)?;
    }
    Ok(())
}

fn request_logs(path: &Path) -> Result<Vec<(PathBuf, u64)>> {
    if path.is_dir() {
        let mut logs = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let directory = entry.path().join("kraai-controller");
            let events = directory.join("proxy.events.jsonl");
            if events.is_file() {
                let metrics: kraai_eval::ProxyMetrics =
                    serde_json::from_slice(&std::fs::read(directory.join("proxy-metrics.json"))?)?;
                logs.push((
                    events,
                    metrics.requests.saturating_add(metrics.unrecorded_requests),
                ));
            }
        }
        logs.sort_by(|left, right| left.0.cmp(&right.0));
        return Ok(logs);
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if value.get("runs").is_some() {
        let suite: SuiteResult = serde_json::from_value(value)?;
        let root = super::report_cache_root(path, &suite.artifact_path)?;
        let mut logs = Vec::new();
        for run in suite.runs {
            if let Some(artifact) = run.artifact_path {
                super::ensure_safe_artifact(&artifact)?;
                logs.extend(request_logs(&root.join(artifact).join("result.json"))?);
            }
        }
        return Ok(logs);
    }
    let result: kraai_eval::RunResult = serde_json::from_value(value)?;
    let Some(proxy) = result.metrics.proxy else {
        return Ok(vec![]);
    };
    Ok(vec![(
        path.with_file_name("proxy.events.jsonl"),
        proxy.requests.saturating_add(proxy.unrecorded_requests),
    )])
}

pub(super) fn format_accounting(accounting: &AccountingSummary) -> String {
    let context = &accounting.context;
    let cost = accounting.estimated_cost.map_or_else(
        || {
            format!(
                "unknown; known portion ~{}, {} unpriced, {} unrecorded",
                accounting.known_estimated_cost,
                accounting.unpriced_requests,
                accounting.unrecorded_requests
            )
        },
        |cost| format!("~{cost}"),
    );
    format!(
        "Estimated API cost: {cost}\nInput context: {} mean, {} peak, {} last recorded; {} measured / {} model requests",
        context
            .mean_input_tokens
            .map_or_else(|| String::from("unknown"), |value| format!("{value:.1}")),
        context
            .peak_input_tokens
            .map_or_else(|| String::from("unknown"), |value| value.to_string()),
        context
            .last_input_tokens
            .map_or_else(|| String::from("unknown"), |value| value.to_string()),
        context.samples,
        accounting.model_requests
    )
}
