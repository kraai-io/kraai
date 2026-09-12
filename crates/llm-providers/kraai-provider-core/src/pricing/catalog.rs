use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use kraai_types::{TokenRates, TokenUsage, Usd};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_AGE: u64 = 24 * 60 * 60;

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    fetched_at: u64,
    providers: BTreeMap<String, CatalogProvider>,
}

#[derive(Serialize, Deserialize)]
struct CatalogProvider {
    api: Option<String>,
    models: BTreeMap<String, CatalogModel>,
}

#[derive(Serialize, Deserialize)]
struct CatalogModel {
    cost: Option<Value>,
}

#[derive(Default)]
pub(super) struct Catalog {
    snapshot: RwLock<Snapshot>,
    attempted_at: AtomicU64,
}

impl Catalog {
    pub async fn load(&self) {
        let Some(path) = cache_path() else { return };
        if let Ok(metadata) = tokio::fs::metadata(&path).await
            && metadata.len() <= MAX_BYTES as u64
            && let Ok(bytes) = tokio::fs::read(path).await
            && let Ok(snapshot) = serde_json::from_slice::<Snapshot>(&bytes)
        {
            *self.snapshot.write().await = snapshot;
        }
    }

    pub async fn refresh(&self) {
        let now = super::now();
        let fetched_at = self.snapshot.read().await.fetched_at;
        if fetched_at <= now && now - fetched_at < MAX_AGE {
            return;
        }
        let last = self.attempted_at.load(Ordering::Relaxed);
        if (now >= last && now - last < 3600)
            || self
                .attempted_at
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        if let Err(error) = self.fetch().await {
            tracing::debug!(%error, "Could not refresh model pricing; retaining cached prices");
        }
    }

    async fn fetch(&self) -> color_eyre::Result<()> {
        let client = crate::build_finite_http_client()?;
        let mut response = client
            .get("https://models.dev/api.json")
            .header(reqwest::header::USER_AGENT, "kraai/0.1")
            .send()
            .await?
            .error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                return Err(color_eyre::eyre::eyre!(
                    "Pricing catalog exceeds size limit"
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let providers: BTreeMap<String, CatalogProvider> = serde_json::from_slice(&bytes)?;
        if providers.is_empty() {
            return Err(color_eyre::eyre::eyre!("Pricing catalog is empty"));
        }
        let snapshot = Snapshot {
            fetched_at: super::now(),
            providers,
        };
        let bytes = serde_json::to_vec(&snapshot)?;
        *self.snapshot.write().await = snapshot;
        if let Some(path) = cache_path() {
            tokio::task::spawn_blocking(move || -> color_eyre::Result<()> {
                use std::io::Write;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut file = atomic_write_file::AtomicWriteFile::open(path)?;
                file.write_all(&bytes)?;
                file.commit()?;
                Ok(())
            })
            .await??;
        }
        Ok(())
    }

    pub async fn lookup(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<(Value, String, u64)> {
        let snapshot = self.snapshot.read().await;
        let (provider_id, provider) = if let Some(id) = provider {
            snapshot.providers.get_key_value(id)?
        } else {
            let api = api?.trim_end_matches('/');
            let mut matches = snapshot.providers.iter().filter(|(_, provider)| {
                provider
                    .api
                    .as_deref()
                    .is_some_and(|url| url.trim_end_matches('/') == api)
            });
            let matched = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            matched
        };
        let cost = provider.models.get(model)?.cost.as_ref()?;
        Some((
            cost.clone(),
            format!("models.dev/{provider_id}/{model}"),
            snapshot.fetched_at,
        ))
    }
}

fn cache_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.cache_dir().join("kraai/pricing.json"))
}

pub(super) fn parse_rates(cost: &Value, usage: &TokenUsage) -> Option<TokenRates> {
    let object = cost.as_object()?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "input"
                | "output"
                | "cache_read"
                | "cache_write"
                | "reasoning"
                | "input_audio"
                | "output_audio"
                | "tiers"
                | "context_over_200k"
        )
    }) {
        return None;
    }
    let prompt = usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens) as u64;
    let mut selected = cost;
    let mut selected_threshold = 0;
    if let Some(tiers) = cost.get("tiers") {
        for entry in tiers.as_array()? {
            let tier = entry.get("tier")?.as_object()?;
            if tier
                .keys()
                .any(|key| !matches!(key.as_str(), "type" | "size"))
                || tier
                    .get("type")
                    .is_some_and(|value| value.as_str() != Some("context"))
            {
                return None;
            }
            let threshold = tier.get("size")?.as_u64()?;
            if prompt > threshold && threshold >= selected_threshold {
                selected = entry;
                selected_threshold = threshold;
            }
        }
    } else if prompt > 200_000
        && let Some(tier) = cost.get("context_over_200k")
    {
        selected = tier;
    }
    for key in ["input", "output", "cache_read", "cache_write", "reasoning"] {
        if selected
            .get(key)
            .is_some_and(|value| value.as_f64().and_then(Usd::from_dollars).is_none())
        {
            return None;
        }
    }
    let rate = |key| {
        selected
            .get(key)
            .and_then(Value::as_f64)
            .and_then(Usd::from_dollars)
    };
    Some(TokenRates {
        input: rate("input")?,
        output: rate("output")?,
        cache_read: rate("cache_read"),
        cache_write: rate("cache_write"),
        reasoning: rate("reasoning"),
    })
}

#[cfg(test)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "tests combine fallible fixture setup with assertions"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matches_serving_endpoint_and_context_tier() -> color_eyre::Result<()> {
        let catalog = Catalog::default();
        *catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
            "fetched_at": 123,
            "providers": {
                "direct": {"api":"https://direct.test/v1", "models":{"model":{"cost":{"input":2,"output":8,"cache_read":0.5,"tiers":[{"tier":{"size":200000},"input":4,"output":12,"cache_read":1}]}}}},
                "reseller": {"api":"https://reseller.test/v1", "models":{"model":{"cost":{"input":1,"output":3}}}}
            }
        }))?;
        let usage = TokenUsage {
            input_tokens: 200_000,
            cache_read_tokens: 1,
            ..Default::default()
        };
        let direct = catalog
            .lookup(None, Some("https://direct.test/v1/"), "model")
            .await;
        assert!(direct.is_some_and(|(cost, source, timestamp)| {
            parse_rates(&cost, &usage).is_some_and(|rates| rates.input == Usd(4_000_000_000))
                && source == "models.dev/direct/model"
                && timestamp == 123
        }));
        let reseller = catalog.lookup(Some("reseller"), None, "model").await;
        assert!(reseller.is_some_and(|(cost, _, _)| {
            parse_rates(&cost, &usage).is_some_and(|rates| rates.input == Usd(1_000_000_000))
        }));
        assert!(
            catalog
                .lookup(None, Some("https://unknown.test/v1"), "model")
                .await
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn reasoning_variant_uses_base_catalog_rates_for_all_token_categories()
    -> color_eyre::Result<()> {
        use crate::{DynamicConfig, ProviderConfig, ProviderManagerConfig, ProviderStreamEvent};
        use futures::StreamExt;
        use kraai_types::{ModelId, ProviderId};

        let provider = ProviderId::new("codex");
        let pricing = super::super::Pricing::new(&ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: provider.clone(),
                type_id: "openai-codex".into(),
                config: DynamicConfig::new(),
            }],
            models: vec![],
        })?;
        *pricing.catalog.snapshot.write().await = serde_json::from_value(serde_json::json!({
            "fetched_at": 123,
            "providers": {
                "openai": {"models": {"gpt-6-astra": {"cost": {
                    "input": 10, "output": 50, "cache_read": 1, "cache_write": 12.5,
                    "tiers": [{"tier": {"type": "context", "size": 272000},
                        "input": 20, "output": 75, "cache_read": 2, "cache_write": 25}]
                }}}}
            }
        }))?;
        for (input_tokens, expected) in [(100, Usd(9_025_000)), (272001, Usd(5_452_320_000))] {
            let source = futures::stream::iter([Ok(ProviderStreamEvent::Usage(TokenUsage {
                input_tokens,
                cache_read_tokens: 400,
                cache_write_tokens: 10,
                output_tokens: 100,
                reasoning_tokens: 50,
                ..Default::default()
            }))])
            .boxed();
            let mut stream = pricing
                .apply(
                    &provider,
                    &ModelId::new("gpt-6-astra-low"),
                    &ModelId::new("gpt-6-astra"),
                    source,
                )
                .await;
            let Some(Ok(ProviderStreamEvent::Usage(usage))) = stream.next().await else {
                return Err(color_eyre::eyre::eyre!("missing usage"));
            };
            assert_eq!(usage.cost.as_ref().map(|cost| cost.amount), Some(expected));
            assert_eq!(
                usage.cost.as_ref().map(|cost| cost.source.as_str()),
                Some("models.dev/openai/gpt-6-astra")
            );
        }
        Ok(())
    }

    #[test]
    fn unknown_pricing_conditions_are_not_silently_ignored() {
        let cost = serde_json::json!({"input":1,"output":2,"tiers":[{}]});
        assert!(parse_rates(&cost, &TokenUsage::default()).is_none());
    }
}
