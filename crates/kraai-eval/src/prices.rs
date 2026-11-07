use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use kraai_types::{TokenRates, TokenUsage, Usd};
use serde::{Deserialize, Serialize};

use crate::{EvalRoot, RequestAccounting, UsageMetrics};

const PRICES_FILE: &str = "prices.json";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl ModelPrice {
    pub fn rates(&self) -> Result<TokenRates> {
        let usd = |name: &str, value: f64| {
            Usd::from_dollars(value).ok_or_else(|| {
                eyre!("{name} price must be a non-negative USD amount per million tokens")
            })
        };
        let optional =
            |name: &str, value: Option<f64>| value.map(|value| usd(name, value)).transpose();
        Ok(TokenRates {
            input: usd("input", self.input)?,
            output: usd("output", self.output)?,
            cache_read: optional("cache read", self.cache_read)?,
            cache_write: optional("cache write", self.cache_write)?,
            reasoning: None,
        })
    }
}

impl std::fmt::Display for ModelPrice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${} input, ${} output", self.input, self.output)?;
        if let Some(price) = self.cache_read {
            write!(f, ", ${price} cache read")?;
        }
        if let Some(price) = self.cache_write {
            write!(f, ", ${price} cache write")?;
        }
        f.write_str(" per million tokens")
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PriceFile {
    schema_version: u32,
    models: BTreeMap<String, ModelPrice>,
}

#[derive(Debug, Clone, Default)]
pub struct Prices {
    models: BTreeMap<String, TokenRates>,
}

impl Prices {
    pub fn load(eval_dir: &Path) -> Result<Self> {
        let models = read_prices(&eval_dir.join(PRICES_FILE))?
            .into_iter()
            .map(|(model, price)| {
                let rates = price
                    .rates()
                    .wrap_err_with(|| format!("saved price for {model}"))?;
                Ok((model, rates))
            })
            .collect::<Result<_>>()?;
        Ok(Self { models })
    }

    pub fn rates(&self, model: &str) -> Option<&TokenRates> {
        self.models.get(model)
    }

    pub fn usage_cost(&self, model: &str, usage: &UsageMetrics) -> Option<Usd> {
        self.rates(model)?.estimate(&token_usage(usage)?)
    }

    pub fn accounting_cost(&self, accounting: &RequestAccounting) -> Option<Usd> {
        if accounting.model_requests == 0 || accounting.unrecorded_requests != 0 {
            return None;
        }
        accounting
            .requests
            .iter()
            .try_fold(Usd(0), |total, request| {
                let cost = self.usage_cost(request.model.as_deref()?, request.usage.as_ref()?)?;
                total.0.checked_add(cost.0).map(Usd)
            })
    }
}

impl EvalRoot {
    pub fn model_price(&self, model: &str) -> Result<Option<ModelPrice>> {
        Ok(read_prices(&self.path().join(PRICES_FILE))?.remove(model))
    }

    pub fn save_model_price(&self, model: &str, price: ModelPrice) -> Result<()> {
        price.rates()?;
        let locks = self.cache().join("locks");
        fs::create_dir_all(&locks)?;
        let _lock = kraai_io::lock::FileLock::acquire(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(locks.join("prices.lock"))?,
        )?;
        let path = self.path().join(PRICES_FILE);
        let mut models = read_prices(&path)?;
        models.insert(model.to_owned(), price);
        let file = PriceFile {
            schema_version: SCHEMA_VERSION,
            models,
        };
        kraai_io::fs::atomic_replace(&path, &serde_json::to_vec_pretty(&file)?)
            .wrap_err_with(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

fn read_prices(path: &Path) -> Result<BTreeMap<String, ModelPrice>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error).wrap_err_with(|| format!("read {}", path.display())),
    };
    let file: PriceFile =
        serde_json::from_slice(&bytes).wrap_err_with(|| format!("parse {}", path.display()))?;
    ensure!(
        file.schema_version == SCHEMA_VERSION,
        "{} has unsupported schema version {}",
        path.display(),
        file.schema_version
    );
    Ok(file.models)
}

fn token_usage(usage: &UsageMetrics) -> Option<TokenUsage> {
    let count = |tokens: u64| usize::try_from(tokens).ok();
    Some(TokenUsage {
        total_tokens: count(usage.total_tokens)?,
        input_tokens: count(usage.input_tokens)?,
        output_tokens: count(usage.output_tokens)?,
        reasoning_tokens: count(usage.reasoning_tokens)?,
        cache_read_tokens: count(usage.cache_read_tokens)?,
        cache_write_tokens: count(usage.cache_write_tokens)?,
        cost: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, cache_read: u64, output: u64) -> UsageMetrics {
        UsageMetrics {
            total_tokens: input + cache_read + output,
            input_tokens: input,
            cache_read_tokens: cache_read,
            output_tokens: output,
            ..UsageMetrics::default()
        }
    }

    #[test]
    fn saved_prices_price_every_request_by_its_model() -> Result<()> {
        let temporary =
            std::env::temp_dir().join(format!("kraai-eval-prices-{}", ulid::Ulid::generate()));
        let root = EvalRoot::open(&temporary)?;
        ensure!(root.model_price("model")?.is_none());
        let price = ModelPrice {
            input: 2.0,
            output: 10.0,
            cache_read: Some(0.5),
            cache_write: None,
        };
        root.save_model_price("model", price)?;
        root.save_model_price(
            "other",
            ModelPrice {
                input: 1.0,
                output: 1.0,
                cache_read: None,
                cache_write: None,
            },
        )?;
        ensure!(root.model_price("model")? == Some(price));
        ensure!(
            root.save_model_price(
                "bad",
                ModelPrice {
                    input: -1.0,
                    ..price
                }
            )
            .is_err()
        );
        let prices = Prices::load(root.path())?;
        ensure!(
            prices.usage_cost("model", &usage(1_000_000, 2_000_000, 100_000))
                == Some(Usd(4_000_000_000))
        );
        ensure!(prices.usage_cost("other", &usage(0, 1, 0)).is_none());
        ensure!(prices.usage_cost("missing", &usage(1, 0, 0)).is_none());
        let request = |model: &str| {
            serde_json::json!({
                "event_index": 0, "model": model,
                "usage": usage(1_000_000, 0, 0),
            })
        };
        let accounting =
            |requests: Vec<serde_json::Value>, unrecorded: u64| -> Result<RequestAccounting> {
                Ok(RequestAccounting {
                    model_requests: requests.len() as u64,
                    unrecorded_requests: unrecorded,
                    requests: requests
                        .into_iter()
                        .map(serde_json::from_value)
                        .collect::<std::result::Result<_, _>>()?,
                    ..RequestAccounting::default()
                })
            };
        ensure!(
            prices.accounting_cost(&accounting(vec![request("model"), request("other")], 0)?)
                == Some(Usd(3_000_000_000))
        );
        ensure!(
            prices
                .accounting_cost(&accounting(vec![request("model"), request("missing")], 0)?)
                .is_none()
        );
        ensure!(
            prices
                .accounting_cost(&accounting(vec![request("model")], 1)?)
                .is_none()
        );
        ensure!(prices.accounting_cost(&accounting(vec![], 0)?).is_none());
        fs::remove_dir_all(temporary)?;
        Ok(())
    }
}
