use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use kraai_types::SessionId;
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, types::ValueRef,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::Semaphore;

const SCHEMA_VERSION: i64 = 2;
const READER_CONNECTIONS: usize = 4;

#[derive(Default)]
pub(crate) struct LeaseTokens(Mutex<BTreeMap<SessionId, i64>>);

impl LeaseTokens {
    fn tokens(&self) -> std::sync::MutexGuard<'_, BTreeMap<SessionId, i64>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn get(&self, session: &SessionId) -> Option<i64> {
        self.tokens().get(session).copied()
    }

    pub(crate) fn insert(&self, session: SessionId, token: i64) {
        self.tokens().insert(session, token);
    }

    pub(crate) fn remove(&self, session: &SessionId) -> Option<i64> {
        self.tokens().remove(session)
    }

    pub(crate) fn snapshot(&self) -> Vec<(SessionId, i64)> {
        self.tokens()
            .iter()
            .map(|(session, token)| (session.clone(), *token))
            .collect()
    }
}

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
    writer: Arc<Mutex<Option<Connection>>>,
    initialized: Arc<AtomicBool>,
    readers: Arc<Mutex<Vec<Connection>>>,
    reader_permits: Arc<Semaphore>,
    leases: Arc<LeaseTokens>,
}

impl Database {
    pub(crate) fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("kraai.sqlite3"),
            writer: Arc::default(),
            initialized: Arc::default(),
            readers: Arc::default(),
            reader_permits: Arc::new(Semaphore::new(READER_CONNECTIONS)),
            leases: Arc::default(),
        }
    }

    pub(crate) fn leases(&self) -> &LeaseTokens {
        &self.leases
    }

    fn open_writer(&self) -> Result<Connection> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| eyre!("Database has no parent"))?;
        kraai_io::fs::create_dir_all(parent)?;
        let mut connection = Connection::open(&self.path)
            .with_context(|| format!("Failed to open database {}", self.path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON;",
        )?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        ensure!(
            (0..=SCHEMA_VERSION).contains(&version),
            "Unsupported SQLite schema version {version}"
        );
        if version == 1 {
            transaction.execute_batch(
                "ALTER TABLE sessions ADD COLUMN lease_token INTEGER NOT NULL DEFAULT 0",
            )?;
        }
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.commit()?;
        Ok(connection)
    }

    fn open_reader(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("Failed to open database {}", self.path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        Ok(connection)
    }

    fn with_writer<T>(
        &self,
        operation: impl FnOnce(&mut Connection, &LeaseTokens) -> Result<T>,
    ) -> Result<T> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_poisoned| eyre!("Database mutex poisoned"))?;
        if writer.is_none() {
            *writer = Some(self.open_writer()?);
            self.initialized.store(true, Ordering::Release);
        }
        let result = operation(
            writer
                .as_mut()
                .ok_or_else(|| eyre!("Database was not initialized"))?,
            &self.leases,
        );
        drop(writer);
        result
    }

    pub(crate) async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection, &LeaseTokens) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let database = self.clone();
        tokio::task::spawn_blocking(move || database.with_writer(operation))
            .await
            .context("Database task failed")?
    }

    pub(crate) async fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection, &LeaseTokens) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self
            .reader_permits
            .clone()
            .acquire_owned()
            .await
            .context("Database reader pool closed")?;
        let database = self.clone();
        tokio::task::spawn_blocking(move || {
            if !database.initialized.load(Ordering::Acquire) {
                database.with_writer(|_, _| Ok(()))?;
            }
            let idle = database
                .readers
                .lock()
                .map_err(|_poisoned| eyre!("Database reader pool poisoned"))?
                .pop();
            let mut connection = match idle {
                Some(connection) => connection,
                None => database.open_reader()?,
            };
            let result = operation(&mut connection, &database.leases);
            if connection.is_autocommit() {
                database
                    .readers
                    .lock()
                    .map_err(|_poisoned| eyre!("Database reader pool poisoned"))?
                    .push(connection);
            }
            drop(permit);
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
    session: &SessionId,
) -> Result<()> {
    let ownership: Option<(bool, i64)> = connection
        .query_row(
            "SELECT lease_active, lease_token FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((active, token)) = ownership {
        if let Some(expected) = leases.get(session) {
            ensure!(
                active && expected == token,
                kraai_execution_types::DomainError::conflict(format!(
                    "Ownership of session {session} was lost"
                ))
            );
        } else {
            ensure!(
                !active,
                kraai_execution_types::DomainError::conflict(format!(
                    "Session {session} is owned by another runtime"
                ))
            );
        }
    } else {
        ensure!(
            leases.get(session).is_none(),
            "Owned session {session} was deleted"
        );
    }
    Ok(())
}

pub(crate) fn bump_revision(connection: &Connection, session: &SessionId) -> Result<()> {
    connection.execute(
        "UPDATE sessions SET revision = revision + 1 WHERE id = ?1",
        [session.as_str()],
    )?;
    Ok(())
}

pub(crate) fn read_record<T: DeserializeOwned>(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<T>> {
    let mut statement =
        connection.prepare_cached("SELECT data FROM records WHERE kind = ?1 AND id = ?2")?;
    let mut rows = statement.query(params![kind, id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let value = row.get_ref(0)?;
    let ValueRef::Text(json) = value else {
        return Err(rusqlite::Error::InvalidColumnType(0, "data".into(), value.data_type()).into());
    };
    let json = std::str::from_utf8(json).map_err(|error| rusqlite::Error::Utf8Error(0, error))?;
    Ok(Some(serde_json::from_str(json)?))
}

pub(crate) fn parse_session_id(value: String) -> Result<SessionId> {
    SessionId::try_new(value).map_err(|error| eyre!("Invalid stored session id: {error}"))
}

pub(crate) fn record_session(
    connection: &Connection,
    kind: &str,
    id: &str,
) -> Result<Option<SessionId>> {
    connection
        .query_row(
            "SELECT session_id FROM records WHERE kind = ?1 AND id = ?2",
            params![kind, id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .map(parse_session_id)
        .transpose()
}

pub(crate) fn write_record(
    connection: &Connection,
    kind: &str,
    id: &str,
    session: Option<&SessionId>,
    value: &impl Serialize,
) -> Result<()> {
    let json = serde_json::to_string(value)?;
    connection.execute(
        "INSERT INTO records(kind, id, session_id, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(kind, id) DO UPDATE SET data = excluded.data,
         session_id = COALESCE(records.session_id, excluded.session_id)",
        params![kind, id, session.map(SessionId::as_str), json],
    )?;
    if let Some(session) = session {
        bump_revision(connection, session)?;
    }
    Ok(())
}

pub(crate) fn list_records<T: DeserializeOwned>(
    connection: &Connection,
    kind: &str,
    session: Option<&SessionId>,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(
        "SELECT data FROM records WHERE kind = ?1 AND (?2 IS NULL OR session_id = ?2) ORDER BY id",
    )?;
    let rows = statement.query_map(params![kind, session.map(SessionId::as_str)], |row| {
        row.get::<_, String>(0)
    })?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}
