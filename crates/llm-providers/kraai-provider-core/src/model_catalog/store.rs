use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};

pub(super) const SCHEMA_VERSION: i64 = 1;
const READ_TIMEOUT: Duration = Duration::from_millis(250);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn cache_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.cache_dir().join("kraai/models.sqlite3"))
}

pub(super) fn open(path: &Path) -> Option<(Connection, u64)> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::default().difference(OpenFlags::SQLITE_OPEN_CREATE),
    )
    .ok()?;
    connection.busy_timeout(READ_TIMEOUT).ok()?;
    connection.execute_batch("PRAGMA query_only = ON").ok()?;
    let fetched_at = fetched_at(&connection).ok()??;
    Some((connection, fetched_at))
}

pub(super) fn fetched_at(connection: &Connection) -> rusqlite::Result<Option<u64>> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Ok(None);
    }
    connection
        .query_row("SELECT fetched_at FROM meta", [], |row| row.get(0))
        .optional()
        .map(|fetched_at| fetched_at.map(i64::cast_unsigned))
}

pub(super) fn write(
    path: Option<&Path>,
    bytes: &[u8],
    fetched_at: u64,
) -> color_eyre::Result<Connection> {
    let Some(path) = path else {
        let mut connection = Connection::open_in_memory()?;
        replace(&mut connection, bytes, fetched_at)?;
        return Ok(connection);
    };
    if let Some(directory) = path.parent() {
        kraai_io::fs::create_dir_all(directory)?;
    }
    match write_file(path, bytes, fetched_at) {
        Err(error) if is_corrupt(&error) => {
            remove(path)?;
            write_file(path, bytes, fetched_at)
        }
        result => result,
    }
}

fn write_file(path: &Path, bytes: &[u8], fetched_at: u64) -> color_eyre::Result<Connection> {
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(WRITE_TIMEOUT)?;
    connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL")?;
    if replace(&mut connection, bytes, fetched_at)? {
        let legacy = path.with_file_name("models.json");
        if let Err(error) = std::fs::remove_file(&legacy)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%error, path = %legacy.display(), "Could not remove the old model catalog cache");
        }
    }
    connection.execute_batch("PRAGMA query_only = ON")?;
    Ok(connection)
}

fn replace(connection: &mut Connection, bytes: &[u8], fetched_at: u64) -> color_eyre::Result<bool> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let rebuild = version != SCHEMA_VERSION;
    if rebuild {
        drop_everything(&transaction)?;
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    } else {
        transaction.execute_batch(
            "DELETE FROM meta; DELETE FROM providers; DELETE FROM models; DELETE FROM cost_tiers;",
        )?;
    }
    super::source::insert(&transaction, bytes)?;
    transaction.execute(
        "INSERT INTO meta (id, fetched_at) VALUES (0, ?1)",
        [fetched_at.cast_signed()],
    )?;
    transaction.commit()?;
    Ok(rebuild)
}

fn drop_everything(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    let objects = transaction
        .prepare(
            "SELECT type, name FROM sqlite_schema
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, name) in objects {
        let kind = if kind == "view" { "VIEW" } else { "TABLE" };
        transaction.execute_batch(&format!(
            "DROP {kind} IF EXISTS \"{}\"",
            name.replace('"', "\"\"")
        ))?;
    }
    Ok(())
}

fn is_corrupt(error: &color_eyre::Report) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .and_then(rusqlite::Error::sqlite_error_code)
        .is_some_and(|code| matches!(code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt))
}

fn remove(path: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = OsString::from(path.as_os_str());
        file.push(suffix);
        match std::fs::remove_file(PathBuf::from(file)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            Ok(()) | Err(_) => {}
        }
    }
    Ok(())
}
