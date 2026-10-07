use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) type LeaseTokens = BTreeMap<String, i64>;

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
    state: Arc<Mutex<DatabaseState>>,
}

#[derive(Default)]
struct DatabaseState {
    connection: Option<Connection>,
    leases: LeaseTokens,
}

impl Database {
    pub(crate) fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("kraai.sqlite3"),
            state: Arc::default(),
        }
    }

    pub(crate) async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection, &mut LeaseTokens) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let database = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = database
                .state
                .lock()
                .map_err(|_poisoned| eyre!("Database mutex poisoned"))?;
            if state.connection.is_none() {
                let parent = database
                    .path
                    .parent()
                    .ok_or_else(|| eyre!("Database has no parent"))?;
                kraai_io::fs::create_dir_all(parent)?;
                let connection = Connection::open(&database.path).with_context(|| {
                    format!("Failed to open database {}", database.path.display())
                })?;
                connection.busy_timeout(Duration::from_secs(5))?;
                connection.execute_batch(include_str!("schema.sql"))?;
                state.connection = Some(connection);
            }
            let DatabaseState { connection, leases } = &mut *state;
            let result = operation(
                connection
                    .as_mut()
                    .ok_or_else(|| eyre!("Database was not initialized"))?,
                leases,
            );
            drop(state);
            result
        })
        .await
        .context("Database task failed")?
    }

    pub(crate) async fn transaction<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Transaction<'_>, &LeaseTokens) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.run(move |connection, leases| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let result = operation(&transaction, leases)?;
            transaction.commit()?;
            Ok(result)
        })
        .await
    }
}

pub(crate) fn now_nanos() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

pub(crate) fn assert_owner(
    connection: &Connection,
    leases: &LeaseTokens,
    session: &str,
) -> Result<()> {
    let ownership: Option<(bool, i64)> = connection
        .query_row(
            "SELECT lease_active, lease_expires_at FROM sessions WHERE id = ?1",
            [session],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((active, expiry)) = ownership {
        if let Some(expected) = leases.get(session) {
            ensure!(
                active && *expected == expiry,
                kraai_types::DomainError::conflict(format!(
                    "Ownership of session {session} was lost"
                ))
            );
        } else {
            ensure!(
                !active,
                kraai_types::DomainError::conflict(format!(
                    "Session {session} is owned by another runtime"
                ))
            );
        }
    } else {
        ensure!(
            !leases.contains_key(session),
            "Owned session {session} was deleted"
        );
    }
    Ok(())
}

pub(crate) fn bump_revision(connection: &Connection, session: &str) -> Result<()> {
    connection.execute(
        "UPDATE sessions SET revision = revision + 1 WHERE id = ?1",
        [session],
    )?;
    Ok(())
}

pub(crate) fn read_record<T: DeserializeOwned>(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<T>> {
    let json: Option<String> = connection
        .query_row(
            "SELECT data FROM records WHERE kind = ?1 AND id = ?2",
            params![kind, id],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

pub(crate) fn record_session(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT session_id FROM records WHERE kind = ?1 AND id = ?2",
            params![kind, id],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

pub(crate) fn write_record(
    connection: &Connection,
    kind: &str,
    id: &str,
    session: Option<&str>,
    value: &impl Serialize,
) -> Result<()> {
    let json = serde_json::to_string(value)?;
    connection.execute(
        "INSERT INTO records(kind, id, session_id, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(kind, id) DO UPDATE SET data = excluded.data,
         session_id = COALESCE(records.session_id, excluded.session_id)",
        params![kind, id, session, json],
    )?;
    if let Some(session) = session {
        bump_revision(connection, session)?;
    }
    Ok(())
}

pub(crate) fn list_records<T: DeserializeOwned>(
    connection: &Connection,
    kind: &str,
    session: Option<&str>,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(
        "SELECT data FROM records WHERE kind = ?1 AND (?2 IS NULL OR session_id = ?2) ORDER BY id",
    )?;
    let rows = statement.query_map(params![kind, session], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}
