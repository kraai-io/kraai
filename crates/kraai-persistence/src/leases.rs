use crate::SqliteSessionStore;
use crate::database::now_nanos;
use color_eyre::eyre::{Result, ensure, eyre};
use kraai_types::SessionId;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::time::Duration;

pub const TURN_LEASE_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionObservation {
    pub revision: i64,
    pub lease_active: bool,
    pub lease_expires_at: i64,
}

fn ensure_claimable(connection: &Connection, session: &SessionId) -> Result<i64> {
    let (active, expires_at): (bool, i64) = connection
        .query_row(
            "SELECT lease_active, lease_expires_at FROM sessions WHERE id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            eyre!(kraai_types::DomainError::not_found(format!(
                "Session not found: {session}"
            )))
        })?;
    let now = now_nanos()?;
    ensure!(
        !active || expires_at < now,
        kraai_types::DomainError::conflict(format!(
            "Session {session} is already running in another instance"
        ))
    );
    Ok(now)
}

fn lease_expiry(now: i64, duration: Duration) -> Result<i64> {
    now.checked_add(i64::try_from(duration.as_nanos())?)
        .ok_or_else(|| eyre!("Lease expiry overflow"))
}

impl SqliteSessionStore {
    pub async fn observe(&self, session_id: &SessionId) -> Result<Option<SessionObservation>> {
        let session = session_id.clone();
        self.database
            .read(move |connection, _| {
                Ok(connection
                    .query_row(
                        "SELECT revision, lease_active AND lease_expires_at >= ?2, lease_expires_at
                         FROM sessions WHERE id = ?1",
                        params![session.as_str(), now_nanos()?],
                        |row| {
                            Ok(SessionObservation {
                                revision: row.get(0)?,
                                lease_active: row.get(1)?,
                                lease_expires_at: row.get(2)?,
                            })
                        },
                    )
                    .optional()?)
            })
            .await
    }

    pub async fn ensure_writable(&self, session_id: &SessionId) -> Result<()> {
        let session = session_id.clone();
        self.database
            .read(move |connection, leases| {
                crate::database::assert_owner(connection, leases, &session)
            })
            .await
    }

    pub async fn owns_turn(&self, session_id: &SessionId) -> Result<bool> {
        let Some(expected) = self.database.leases().get(session_id) else {
            return Ok(false);
        };
        let session = session_id.clone();
        self.database
            .read(move |connection, _| {
                Ok(connection
                    .query_row(
                        "SELECT lease_active AND lease_token = ?2 FROM sessions WHERE id = ?1",
                        params![session.as_str(), expected],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(false))
            })
            .await
    }

    pub async fn claim_turn(&self, session_id: &SessionId) -> Result<()> {
        self.claim_turn_for(session_id, TURN_LEASE_DURATION).await
    }

    pub async fn claim_turn_for(&self, session_id: &SessionId, duration: Duration) -> Result<()> {
        ensure!(!duration.is_zero(), "Lease duration must be positive");
        let session = session_id.clone();
        self.database
            .run(move |connection, leases| {
                ensure_claimable(connection, &session)?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = ensure_claimable(&transaction, &session)?;
                let token: i64 = transaction.query_row(
                    "UPDATE sessions SET lease_active = 1, lease_token = lease_token + 1,
                     lease_expires_at = ?2, revision = revision + 1
                     WHERE id = ?1 RETURNING lease_token",
                    params![session.as_str(), lease_expiry(now, duration)?],
                    |row| row.get(0),
                )?;
                transaction.commit()?;
                leases.insert(session, token);
                Ok(())
            })
            .await
    }

    pub async fn renew_turn(&self, session_id: &SessionId) -> Result<()> {
        let expected = self
            .database
            .leases()
            .get(session_id)
            .ok_or_else(|| eyre!("Session is not owned by this runtime"))?;
        let session = session_id.clone();
        self.database
            .run(move |connection, _| {
                let updated = connection.execute(
                    "UPDATE sessions SET lease_expires_at = ?3
                     WHERE id = ?1 AND lease_active = 1 AND lease_token = ?2",
                    params![
                        session.as_str(),
                        expected,
                        lease_expiry(now_nanos()?, TURN_LEASE_DURATION)?
                    ],
                )?;
                ensure!(
                    updated == 1,
                    kraai_types::DomainError::conflict(format!(
                        "Ownership of session {session} was lost"
                    ))
                );
                Ok(())
            })
            .await
    }

    pub async fn release_turn(&self, session_id: &SessionId) -> Result<bool> {
        let session = session_id.clone();
        self.database
            .run(move |connection, leases| {
                let Some(expected) = leases.remove(&session) else {
                    return Ok(false);
                };
                let updated = connection.execute(
                    "UPDATE sessions SET lease_active = 0, revision = revision + 1
                     WHERE id = ?1 AND lease_active = 1 AND lease_token = ?2",
                    params![session.as_str(), expected],
                )?;
                Ok(updated == 1)
            })
            .await
    }

    pub fn owned_sessions(&self) -> Vec<SessionId> {
        self.database
            .leases()
            .snapshot()
            .into_iter()
            .map(|(session, _)| session)
            .collect()
    }

    pub fn owned_turns(&self) -> Vec<(SessionId, i64)> {
        self.database.leases().snapshot()
    }

    pub fn lease_token_matches(&self, session_id: &SessionId, expected: i64) -> bool {
        self.database.leases().get(session_id) == Some(expected)
    }
}
