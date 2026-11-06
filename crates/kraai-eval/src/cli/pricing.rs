use clap::Args;
use color_eyre::eyre::{Result, eyre};
use kraai_eval::PricingOptions;
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
