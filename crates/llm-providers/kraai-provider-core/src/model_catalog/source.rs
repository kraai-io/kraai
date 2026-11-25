use std::collections::BTreeMap;

use color_eyre::eyre::ensure;
use kraai_types::{TokenRates, Usd};
use rusqlite::{Transaction, params};
use serde::Deserialize;
use serde::de::{Deserializer, Error, MapAccess, Visitor};
use serde_json::Value;

use super::cost::CatalogCost;

#[derive(Deserialize)]
struct CatalogProvider {
    api: Option<String>,
    #[serde(default)]
    name: Option<String>,
    models: BTreeMap<String, CatalogModel>,
}

#[derive(Deserialize)]
struct CatalogModel {
    #[serde(default)]
    canonical_model_id: Option<String>,
    cost: Option<Value>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    modalities: Option<Modalities>,
    #[serde(default)]
    limit: Option<Limits>,
    #[serde(flatten)]
    options: crate::DiscoveredModelOptions,
}

#[derive(Deserialize)]
struct Modalities {
    input: Vec<String>,
}

#[derive(Deserialize)]
struct Limits {
    context: Option<usize>,
    #[serde(default)]
    output: Option<usize>,
}

pub(super) fn insert(transaction: &Transaction<'_>, bytes: &[u8]) -> color_eyre::Result<()> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let providers = deserializer.deserialize_map(Providers { transaction })?;
    deserializer.end()?;
    ensure!(providers > 0, "Model catalog is empty");
    Ok(())
}

struct Providers<'a> {
    transaction: &'a Transaction<'a>,
}

impl<'de> Visitor<'de> for Providers<'_> {
    type Value = usize;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a map of model providers")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<usize, A::Error> {
        let mut providers = 0;
        while let Some(id) = map.next_key::<String>()? {
            let provider: CatalogProvider = map.next_value()?;
            insert_provider(self.transaction, &id, provider).map_err(A::Error::custom)?;
            providers += 1;
        }
        Ok(providers)
    }
}

fn insert_provider(
    transaction: &Transaction<'_>,
    id: &str,
    provider: CatalogProvider,
) -> color_eyre::Result<()> {
    transaction
        .prepare_cached("DELETE FROM models WHERE provider = ?1")?
        .execute([id])?;
    transaction
        .prepare_cached("DELETE FROM cost_tiers WHERE provider = ?1")?
        .execute([id])?;
    transaction
        .prepare_cached(
            "INSERT OR REPLACE INTO providers (id, api, folded_name) VALUES (?1, ?2, ?3)",
        )?
        .execute(params![
            id,
            provider.api.as_deref().map(|api| api.trim_end_matches('/')),
            provider.name.as_deref().map(str::to_ascii_lowercase),
        ])?;
    let mut models = transaction.prepare_cached(
        "INSERT INTO models (provider, model, canonical_model_id, name, max_context, max_output,
             supports_images, options, priced, unknown_cost_dimensions,
             input, output, cache_read, cache_write, reasoning)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut tiers = transaction.prepare_cached(
        "INSERT INTO cost_tiers (provider, model, position, size,
             input, output, cache_read, cache_write, reasoning)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for (model_id, model) in provider.models {
        let cost = model.cost.as_ref().map(CatalogCost::from_models_dev);
        let limit = |field: fn(&Limits) -> Option<usize>| {
            model
                .limit
                .as_ref()
                .and_then(field)
                .filter(|limit| *limit > 0)
                .map(|limit| (limit as u64).cast_signed())
        };
        let options = serde_json::to_string(&model.options)?;
        let [input, output, cache_read, cache_write, reasoning] =
            rate_columns(cost.as_ref().and_then(|cost| cost.as_ref()?.base.as_ref()));
        models.execute(params![
            id,
            model_id,
            model.canonical_model_id,
            model.name,
            limit(|limits| limits.context),
            limit(|limits| limits.output),
            model
                .modalities
                .as_ref()
                .map(|modalities| modalities.input.iter().any(|input| input == "image")),
            (options != "{}").then_some(options),
            cost.is_some(),
            cost.as_ref().is_some_and(Option::is_none),
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        ])?;
        for (tier, position) in cost
            .iter()
            .flatten()
            .flat_map(|cost| &cost.tiers)
            .zip(0_i64..)
        {
            let [input, output, cache_read, cache_write, reasoning] =
                rate_columns(tier.rates.as_ref());
            tiers.execute(params![
                id,
                model_id,
                position,
                tier.size.cast_signed(),
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            ])?;
        }
    }
    Ok(())
}

fn rate_columns(rates: Option<&TokenRates>) -> [Option<i64>; 5] {
    let column = |rate: Option<Usd>| rate.map(|rate| rate.0.cast_signed());
    match rates {
        Some(rates) => [
            column(Some(rates.input)),
            column(Some(rates.output)),
            column(rates.cache_read),
            column(rates.cache_write),
            column(rates.reasoning),
        ],
        None => [None; 5],
    }
}
