use crate::database::{LeaseTokens, assert_owner, bump_revision, record_session, write_record};
use color_eyre::eyre::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashSet;

fn message_sessions(
    connection: &Connection,
    id: &str,
    associated_session: Option<&str>,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "WITH RECURSIVE descendants(id) AS (
                SELECT ?1
                UNION SELECT records.id FROM descendants CROSS JOIN records
                    WHERE records.kind = 'message' AND json_valid(records.data)
                    AND json_extract(records.data, '$.parent_id') = +descendants.id
            ) SELECT id FROM sessions WHERE tip_id IN (SELECT id FROM descendants)
            UNION SELECT id FROM sessions WHERE id = ?2
            UNION SELECT sessions.id FROM sessions JOIN records ON sessions.id = records.session_id
                WHERE records.kind = 'message' AND records.id = ?1
            ORDER BY 1",
    )?;
    Ok(statement
        .query_map(params![id, associated_session], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn assert_message_owners(
    connection: &Connection,
    leases: &LeaseTokens,
    id: &str,
    associated_session: Option<&str>,
) -> Result<Vec<String>> {
    let mut cursor = id.to_string();
    let mut associated_session = associated_session;
    let mut visited = HashSet::new();
    loop {
        ensure!(
            visited.insert(cursor.clone()),
            "Corrupt message parent graph: cycle repeats message {cursor}"
        );
        if let Some(session) = associated_session {
            assert_owner(connection, leases, session)?;
        }
        if let Some(session) = record_session(connection, "message", &cursor)?
            && Some(session.as_str()) != associated_session
        {
            assert_owner(connection, leases, &session)?;
        }
        let sessions = message_sessions(connection, &cursor, associated_session)?;
        for session in &sessions {
            assert_owner(connection, leases, session)?;
        }
        if !sessions.is_empty() {
            return Ok(sessions);
        }
        let parent: Option<Option<String>> = connection
            .query_row(
                "SELECT json_extract(data, '$.parent_id') FROM records WHERE kind = 'message' AND id = ?1",
                [&cursor],
                |row| row.get(0),
            )
            .optional()?;
        let Some(parent) = parent.flatten() else {
            return Ok(sessions);
        };
        cursor = parent;
        associated_session = None;
    }
}

pub(crate) fn write_message_record(
    connection: &Connection,
    leases: &LeaseTokens,
    kind: &str,
    id: &str,
    associated_session: Option<&str>,
    value: &impl Serialize,
) -> Result<()> {
    let sessions = assert_message_owners(connection, leases, id, associated_session)?;
    write_record(connection, kind, id, associated_session, value)?;
    for session in sessions {
        if Some(session.as_str()) != associated_session {
            bump_revision(connection, &session)?;
        }
    }
    Ok(())
}

pub(crate) fn delete_message_record(
    connection: &Connection,
    leases: &LeaseTokens,
    kind: &str,
    id: &str,
) -> Result<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM records WHERE kind = ?1 AND id = ?2)",
        params![kind, id],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let associated_session = record_session(connection, kind, id)?;
    let sessions = assert_message_owners(connection, leases, id, associated_session.as_deref())?;
    connection.execute(
        "DELETE FROM records WHERE kind = ?1 AND id = ?2",
        params![kind, id],
    )?;
    for session in sessions {
        bump_revision(connection, &session)?;
    }
    Ok(())
}
