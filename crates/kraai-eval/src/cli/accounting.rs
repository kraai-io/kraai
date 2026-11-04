use std::path::{Path, PathBuf};

use clap::Args;
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_eval::{AccountingSummary, PricingOptions, SuiteResult};
use kraai_types::{TokenRates, Usd};

#[derive(Debug, Clone, Default, Args, serde::Serialize)]
pub(super) struct PricingArgs {
    #[arg(
        long,
        value_name = "USD",
        requires = "price_output",
        help = "Input price per million tokens for every request, instead of catalog prices"
    )]
    price_input: Option<f64>,
    #[arg(
        long,
        value_name = "USD",
        requires = "price_input",
        help = "Output price per million tokens for every request"
    )]
    price_output: Option<f64>,
    #[arg(
        long,
        value_name = "USD",
        requires = "price_input",
        help = "Cached input price per million tokens; uncounted when unset"
    )]
    price_cache_read: Option<f64>,
    #[arg(
        long,
        value_name = "USD",
        requires = "price_input",
        help = "Cache write price per million tokens; uncounted when unset"
    )]
    price_cache_write: Option<f64>,
}

impl PricingArgs {
    pub fn options(&self) -> Result<PricingOptions> {
        let (Some(input), Some(output)) = (self.price_input, self.price_output) else {
            return Ok(PricingOptions::default());
        };
        let usd = |flag: &str, value: f64| {
            Usd::from_dollars(value).ok_or_else(|| {
                eyre!("--{flag} must be a non-negative USD price per million tokens")
            })
        };
        let optional =
            |flag: &str, value: Option<f64>| value.map(|value| usd(flag, value)).transpose();
        Ok(PricingOptions::flat(TokenRates {
            input: usd("price-input", input)?,
            output: usd("price-output", output)?,
            cache_read: optional("price-cache-read", self.price_cache_read)?,
            cache_write: optional("price-cache-write", self.price_cache_write)?,
            reasoning: None,
        }))
    }

    pub fn append(&self, command: &mut Vec<String>) {
        for (flag, value) in [
            ("--price-input", self.price_input),
            ("--price-output", self.price_output),
            ("--price-cache-read", self.price_cache_read),
            ("--price-cache-write", self.price_cache_write),
        ] {
            if let Some(value) = value {
                command.extend([String::from(flag), value.to_string()]);
            }
        }
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
        "Estimated standard-tier API cost: {cost}\nInput context: {} mean, {} peak, {} last recorded; {} measured / {} model requests",
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
