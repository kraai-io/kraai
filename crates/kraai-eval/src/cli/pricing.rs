use clap::Args;
use kraai_eval::ModelPrice;

#[derive(Debug, Clone, Default, Args, serde::Serialize)]
pub(super) struct PricingArgs {
    #[arg(
        long,
        value_name = "USD",
        requires = "price_output",
        help = "Input price per million tokens; saves the model's price for every later run"
    )]
    price_input: Option<f64>,
    #[arg(
        long,
        value_name = "USD",
        requires = "price_input",
        help = "Output price per million tokens, also used for reasoning tokens"
    )]
    price_output: Option<f64>,
    #[arg(
        long,
        value_name = "USD",
        requires = "price_input",
        help = "Cached input price per million tokens; without it, requests that read the cache have no cost"
    )]
    price_cache_read: Option<f64>,
    #[arg(
        long,
        value_name = "USD",
        requires = "price_input",
        help = "Cache write price per million tokens; without it, requests that write the cache have no cost"
    )]
    price_cache_write: Option<f64>,
}

impl PricingArgs {
    pub fn price(&self) -> Option<ModelPrice> {
        Some(ModelPrice {
            input: self.price_input?,
            output: self.price_output?,
            cache_read: self.price_cache_read,
            cache_write: self.price_cache_write,
        })
    }
}
