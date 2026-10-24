use crate::database::Database;
use color_eyre::eyre::Result;
use kraai_types::ProviderId;
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListVersion {
    pub source: String,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredModelList {
    pub version: ModelListVersion,
    pub data: serde_json::Value,
}

/// Provider model lists shared by every process using the database. Times are unix
/// milliseconds supplied by the caller.
#[derive(Clone)]
pub struct SqliteModelListStore {
    database: Database,
}

impl SqliteModelListStore {
    pub(crate) fn with_database(database: Database) -> Self {
        Self { database }
    }

    pub async fn versions(&self) -> Result<BTreeMap<ProviderId, ModelListVersion>> {
        self.database
            .read(|connection, _| {
                let mut statement = connection.prepare_cached(
                    "SELECT provider_id, source, fetched_at FROM model_lists WHERE data IS NOT NULL",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((
                        ProviderId::new(row.get::<_, String>(0)?),
                        ModelListVersion {
                            source: row.get(1)?,
                            fetched_at: row.get(2)?,
                        },
                    ))
                })?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    pub async fn read(&self, provider: &ProviderId) -> Result<Option<StoredModelList>> {
        let provider = provider.to_string();
        self.database
            .read(move |connection, _| {
                let row = connection
                    .query_row(
                        "SELECT source, fetched_at, data FROM model_lists
                         WHERE provider_id = ?1 AND data IS NOT NULL",
                        [provider],
                        |row| {
                            Ok((
                                ModelListVersion {
                                    source: row.get(0)?,
                                    fetched_at: row.get(1)?,
                                },
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                row.map(|(version, data)| {
                    Ok(StoredModelList {
                        version,
                        data: serde_json::from_str(&data)?,
                    })
                })
                .transpose()
            })
            .await
    }

    /// Claims the right to refresh `provider` until `until`. Fails while another claim is live
    /// or once the stored list is no longer `seen`, so a list saved by another process since it
    /// was read is not fetched again.
    pub async fn claim(
        &self,
        provider: &ProviderId,
        seen: Option<&ModelListVersion>,
        now: i64,
        until: i64,
    ) -> Result<bool> {
        let provider = provider.to_string();
        let (source, fetched_at) = seen.map_or((String::new(), 0), |seen| {
            (seen.source.clone(), seen.fetched_at)
        });
        self.database
            .run(move |connection, _| {
                let changed = connection.execute(
                    "INSERT INTO model_lists (provider_id, claimed_until) VALUES (?1, ?3)
                     ON CONFLICT(provider_id) DO UPDATE SET claimed_until = excluded.claimed_until
                     WHERE model_lists.claimed_until <= ?2
                         AND model_lists.source = ?4 AND model_lists.fetched_at = ?5",
                    params![provider, now, until, source, fetched_at],
                )?;
                Ok(changed > 0)
            })
            .await
    }

    /// Gives up a claim made with `claimed_until`, unless another claim replaced it.
    pub async fn release(&self, provider: &ProviderId, claimed_until: i64) -> Result<()> {
        let provider = provider.to_string();
        self.database
            .run(move |connection, _| {
                connection.execute(
                    "UPDATE model_lists SET claimed_until = 0
                     WHERE provider_id = ?1 AND claimed_until = ?2",
                    params![provider, claimed_until],
                )?;
                Ok(())
            })
            .await
    }

    /// Stores a fetched list and releases any claim on it, unless a list fetched later is
    /// already stored.
    pub async fn save(
        &self,
        provider: &ProviderId,
        version: &ModelListVersion,
        data: &serde_json::Value,
    ) -> Result<()> {
        let provider = provider.to_string();
        let ModelListVersion { source, fetched_at } = version.clone();
        let data = serde_json::to_string(data)?;
        self.database
            .run(move |connection, _| {
                connection.execute(
                    "INSERT INTO model_lists (provider_id, source, fetched_at, data, claimed_until)
                     VALUES (?1, ?2, ?3, ?4, 0)
                     ON CONFLICT(provider_id) DO UPDATE SET source = excluded.source,
                         fetched_at = excluded.fetched_at, data = excluded.data, claimed_until = 0
                     WHERE excluded.fetched_at > model_lists.fetched_at",
                    params![provider, source, fetched_at, data],
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
