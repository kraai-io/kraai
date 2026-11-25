use kraai_types::{TokenRates, Usd};
use rusqlite::{Connection, OptionalExtension, Row};

use super::cost::{CatalogCost, CostTier};

const ENTRY_COLUMNS: &str = "provider, model, canonical_model_id, name, max_context, max_output,
    supports_images, options, priced, unknown_cost_dimensions,
    input, output, cache_read, cache_write, reasoning";

pub(super) struct Entry {
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) canonical_model_id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) max_context: Option<usize>,
    pub(super) max_output: Option<usize>,
    pub(super) supports_images: Option<bool>,
    options: Option<String>,
    priced: bool,
    unknown_cost_dimensions: bool,
    base: Option<TokenRates>,
}

pub(super) enum Serving {
    Unknown,
    Unscoped,
    Provider(String),
}

enum Matches {
    None,
    One(String),
    Many,
}

impl Entry {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let limit = |index| -> rusqlite::Result<Option<usize>> {
            Ok(row
                .get::<_, Option<i64>>(index)?
                .map(|limit| limit.cast_unsigned() as usize))
        };
        Ok(Self {
            provider: row.get(0)?,
            model: row.get(1)?,
            canonical_model_id: row.get(2)?,
            name: row.get(3)?,
            max_context: limit(4)?,
            max_output: limit(5)?,
            supports_images: row.get(6)?,
            options: row.get(7)?,
            priced: row.get(8)?,
            unknown_cost_dimensions: row.get(9)?,
            base: rates(row, 10)?,
        })
    }

    pub(super) fn options(&self) -> crate::DiscoveredModelOptions {
        self.options
            .as_deref()
            .and_then(|options| serde_json::from_str(options).ok())
            .unwrap_or_default()
    }

    pub(super) fn cost(&self, connection: &Connection) -> rusqlite::Result<Option<CatalogCost>> {
        if !self.priced {
            return Ok(None);
        }
        if self.unknown_cost_dimensions {
            return Ok(Some(CatalogCost::default()));
        }
        let tiers = connection
            .prepare_cached(
                "SELECT size, input, output, cache_read, cache_write, reasoning FROM cost_tiers
                 WHERE provider = ?1 AND model = ?2 ORDER BY position",
            )?
            .query_map([&self.provider, &self.model], |row| {
                Ok(CostTier {
                    size: row.get::<_, i64>(0)?.cast_unsigned(),
                    rates: rates(row, 1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(CatalogCost {
            base: self.base.clone(),
            tiers,
        }))
    }
}

fn rates(row: &Row<'_>, first: usize) -> rusqlite::Result<Option<TokenRates>> {
    let rate = |offset| {
        Ok::<_, rusqlite::Error>(
            row.get::<_, Option<i64>>(first + offset)?
                .map(|rate| Usd(rate.cast_unsigned())),
        )
    };
    let (Some(input), Some(output)) = (rate(0)?, rate(1)?) else {
        return Ok(None);
    };
    Ok(Some(TokenRates {
        input,
        output,
        cache_read: rate(2)?,
        cache_write: rate(3)?,
        reasoning: rate(4)?,
    }))
}

pub(super) fn serving_provider(
    connection: &Connection,
    provider: Option<&str>,
    api: Option<&str>,
) -> rusqlite::Result<Serving> {
    if let Some(provider) = provider {
        return Ok(if provider_exists(connection, provider)? {
            Serving::Provider(provider.to_owned())
        } else {
            Serving::Unknown
        });
    }
    let Some(api) = api else {
        return Ok(Serving::Unscoped);
    };
    let api = api.trim_end_matches('/');
    match unique_provider(connection, "api", api)? {
        Matches::One(provider) => return Ok(Serving::Provider(provider)),
        Matches::Many => return Ok(Serving::Unknown),
        Matches::None => {}
    }
    if api == "https://api.groq.com/openai/v1" {
        return Ok(if provider_exists(connection, "groq")? {
            Serving::Provider("groq".into())
        } else {
            Serving::Unknown
        });
    }
    Ok(Serving::Unscoped)
}

pub(super) fn serving_model(
    connection: &Connection,
    serving: &Serving,
    model: &str,
) -> rusqlite::Result<Option<Entry>> {
    match serving {
        Serving::Provider(provider) => exact_model(connection, provider, model),
        Serving::Unknown | Serving::Unscoped => Ok(None),
    }
}

pub(super) fn owned_model(
    connection: &Connection,
    serving: &Serving,
    model: &str,
    owner: Option<&str>,
) -> rusqlite::Result<Option<Entry>> {
    let (Serving::Unscoped, Some(owner)) = (serving, owner) else {
        return Ok(None);
    };
    let owner = owner.trim();
    if provider_exists(connection, owner)? {
        return exact_model(connection, owner, model);
    }
    match unique_provider(connection, "folded_name", &owner.to_ascii_lowercase())? {
        Matches::One(provider) => exact_model(connection, &provider, model),
        Matches::None | Matches::Many => Ok(None),
    }
}

pub(super) fn model(
    connection: &Connection,
    serving: &Serving,
    model: &str,
) -> rusqlite::Result<Option<Entry>> {
    match serving {
        Serving::Unknown => return Ok(None),
        Serving::Provider(provider) => return exact_model(connection, provider, model),
        Serving::Unscoped => {}
    }
    if let Some(resolved) = canonical_model(connection, model)? {
        return Ok(Some(resolved));
    }
    let candidates = connection
        .prepare_cached("SELECT provider, canonical_model_id FROM models WHERE model = ?1")?
        .query_map([model], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut resolved = None;
    let mut entry = None;
    let mut verified = Vec::new();
    for (provider, canonical) in candidates {
        let key = match canonical {
            Some(canonical) if verified.contains(&canonical) => continue,
            Some(canonical) => {
                let Some(found) = canonical_model(connection, &canonical)? else {
                    return Ok(None);
                };
                verified.push(canonical);
                let key = (found.provider.clone(), found.model.clone());
                if entry.is_none() {
                    entry = Some(found);
                }
                key
            }
            None => (provider, model.to_owned()),
        };
        match &resolved {
            Some(resolved) if *resolved != key => return Ok(None),
            Some(_) => {}
            None => resolved = Some(key),
        }
    }
    match (entry, resolved) {
        (Some(entry), _) => Ok(Some(entry)),
        (None, Some((provider, model))) => exact_model(connection, &provider, &model),
        (None, None) => Ok(None),
    }
}

pub(super) fn canonical_model(
    connection: &Connection,
    canonical: &str,
) -> rusqlite::Result<Option<Entry>> {
    let Some((provider, model)) = canonical.split_once('/') else {
        return Ok(None);
    };
    Ok(exact_model(connection, provider, model)?.filter(|entry| {
        entry
            .canonical_model_id
            .as_deref()
            .is_none_or(|identity| identity == canonical)
    }))
}

fn exact_model(
    connection: &Connection,
    provider: &str,
    model: &str,
) -> rusqlite::Result<Option<Entry>> {
    connection
        .prepare_cached(&format!(
            "SELECT {ENTRY_COLUMNS} FROM models WHERE provider = ?1 AND model = ?2"
        ))?
        .query_row([provider, model], Entry::from_row)
        .optional()
}

fn provider_exists(connection: &Connection, provider: &str) -> rusqlite::Result<bool> {
    connection
        .prepare_cached("SELECT 1 FROM providers WHERE id = ?1")?
        .exists([provider])
}

fn unique_provider(
    connection: &Connection,
    column: &str,
    value: &str,
) -> rusqlite::Result<Matches> {
    let ids = connection
        .prepare_cached(&format!(
            "SELECT id FROM providers WHERE {column} = ?1 LIMIT 2"
        ))?
        .query_map([value], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut ids = ids.into_iter();
    Ok(match (ids.next(), ids.next()) {
        (None, _) => Matches::None,
        (Some(id), None) => Matches::One(id),
        (Some(_), Some(_)) => Matches::Many,
    })
}
