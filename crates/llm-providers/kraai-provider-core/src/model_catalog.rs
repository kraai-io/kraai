use std::collections::BTreeMap;
use std::io::Read;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modalities: Option<Modalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<Limits>,
}

#[derive(Serialize, Deserialize)]
struct Modalities {
    input: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Limits {
    context: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogModelMetadata {
    pub name: Option<String>,
    pub max_context: Option<usize>,
    pub supports_images: Option<bool>,
}

#[derive(Default)]
pub struct ModelCatalog {
    snapshot: RwLock<Snapshot>,
    attempted_at: AtomicU64,
    initialized: tokio::sync::OnceCell<()>,
}

impl ModelCatalog {
    pub async fn initialize(&self) {
        self.initialized
            .get_or_init(|| async {
                self.load().await;
                self.refresh().await;
            })
            .await;
    }

    async fn load(&self) {
        let Some(path) = cache_path() else { return };
        if let Ok(Some(snapshot)) = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() > MAX_BYTES as u64 {
                return None;
            }
            read_cached_snapshot(file)
        })
        .await
        {
            self.replace_snapshot(snapshot).await;
        }
    }

    pub async fn refresh(&self) {
        let now = crate::pricing::now();
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
            tracing::debug!(%error, "Could not refresh model catalog; retaining cached metadata");
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
                return Err(color_eyre::eyre::eyre!("Model catalog exceeds size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let (snapshot, bytes) = tokio::task::spawn_blocking(move || {
            let providers: BTreeMap<String, CatalogProvider> = serde_json::from_slice(&bytes)?;
            if providers.is_empty() {
                return Err(color_eyre::eyre::eyre!("Model catalog is empty"));
            }
            let snapshot = Snapshot {
                fetched_at: crate::pricing::now(),
                providers,
            };
            let encoded = serde_json::to_vec(&snapshot)?;
            Ok((snapshot, encoded))
        })
        .await??;
        self.replace_snapshot(snapshot).await;
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

    async fn replace_snapshot(&self, snapshot: Snapshot) {
        let mut current = self.snapshot.write().await;
        let previous = std::mem::replace(&mut *current, snapshot);
        drop(current);
        drop(previous);
    }

    pub async fn lookup(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<(Value, String, u64)> {
        let snapshot = self.snapshot.read().await;
        let (provider_id, provider) = snapshot.provider(provider, api)?;
        let cost = provider.models.get(model)?.cost.as_ref()?;
        Some((
            cost.clone(),
            format!("models.dev/{provider_id}/{model}"),
            snapshot.fetched_at,
        ))
    }
}

impl Snapshot {
    fn provider<'a>(
        &'a self,
        provider: Option<&str>,
        api: Option<&str>,
    ) -> Option<(&'a String, &'a CatalogProvider)> {
        let (provider_id, provider) = if let Some(id) = provider {
            self.providers.get_key_value(id)?
        } else {
            let api = api?.trim_end_matches('/');
            let mut matches = self.providers.iter().filter(|(_, provider)| {
                provider
                    .api
                    .as_deref()
                    .is_some_and(|url| url.trim_end_matches('/') == api)
            });
            let matched = matches.next();
            if matches.next().is_some() {
                return None;
            }
            matched.or_else(|| {
                let provider = match api {
                    "https://api.groq.com/openai/v1" => "groq",
                    _ => return None,
                };
                self.providers.get_key_value(provider)
            })?
        };
        Some((provider_id, provider))
    }
}

impl ModelCatalog {
    pub async fn metadata(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<CatalogModelMetadata> {
        let snapshot = self.snapshot.read().await;
        let (_, provider) = snapshot.provider(provider, api)?;
        let model = provider.models.get(model)?;
        let metadata = CatalogModelMetadata {
            name: model.name.clone(),
            max_context: model
                .limit
                .as_ref()
                .and_then(|limit| limit.context)
                .filter(|limit| *limit > 0),
            supports_images: model
                .modalities
                .as_ref()
                .map(|modalities| modalities.input.iter().any(|input| input == "image")),
        };
        drop(snapshot);
        Some(metadata)
    }
}

fn read_cached_snapshot(reader: impl Read) -> Option<Snapshot> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn cache_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.cache_dir().join("kraai/models.json"))
}

pub(crate) fn parse_rates(cost: &Value, usage: &TokenUsage) -> Option<TokenRates> {
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
#[path = "model_catalog_tests.rs"]
mod tests;
