use crate::SqliteSessionStore;
use crate::database::now_nanos;
use color_eyre::eyre::{Result, ensure, eyre};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::time::Duration;

pub const TURN_LEASE_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionObservation {
    pub revision: i64,
    pub lease_active: bool,
    pub lease_expires_at: i64,
}

fn claimable_lease(connection: &Connection, session: &str) -> Result<(i64, i64)> {
    let (active, previous): (bool, i64) = connection
        .query_row(
            "SELECT lease_active, lease_expires_at FROM sessions WHERE id = ?1",
            [session],
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
        !active || previous < now,
        kraai_types::DomainError::conflict(format!(
            "Session {session} is already running in another instance"
        ))
    );
    Ok((previous, now))
}

impl SqliteSessionStore {
    pub async fn observe(&self, session_id: &str) -> Result<Option<SessionObservation>> {
        let session = session_id.to_string();
        self.database.run(move |connection, _| Ok(connection.query_row(
            "SELECT revision, lease_active AND lease_expires_at >= ?2, lease_expires_at FROM sessions WHERE id = ?1", params![session, now_nanos()?],
            |row| Ok(SessionObservation { revision: row.get(0)?, lease_active: row.get(1)?, lease_expires_at: row.get(2)? }),
        ).optional()?)).await
    }

    pub async fn ensure_writable(&self, session_id: &str) -> Result<()> {
        let session = session_id.to_string();
        self.database
            .run(move |connection, leases| {
                crate::database::assert_owner(connection, leases, &session)
            })
            .await
    }

    pub async fn owns_turn(&self, session_id: &str) -> Result<bool> {
        let session = session_id.to_string();
        self.database
            .run(move |connection, leases| {
                let Some(expected) = leases.get(&session) else {
                    return Ok(false);
                };
                Ok(connection
                    .query_row(
                        "SELECT lease_active AND lease_expires_at = ?2 FROM sessions WHERE id = ?1",
                        params![session, expected],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(false))
            })
            .await
    }

    pub async fn claim_turn(&self, session_id: &str) -> Result<()> {
        self.claim_turn_for(session_id, TURN_LEASE_DURATION).await
    }

    pub async fn claim_turn_for(&self, session_id: &str, duration: Duration) -> Result<()> {
        ensure!(!duration.is_zero(), "Lease duration must be positive");
        let session = session_id.to_string();
        self.database.run(move |connection, leases| {
            claimable_lease(connection, &session)?;
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (previous, now) = claimable_lease(&transaction, &session)?;
            let expiry = now.checked_add(i64::try_from(duration.as_nanos())?).ok_or_else(|| eyre!("Lease expiry overflow"))?;
            ensure!(expiry > previous, kraai_types::DomainError::conflict("Clock has not advanced enough to issue a new session lease"));
            transaction.execute("UPDATE sessions SET lease_active = 1, lease_expires_at = ?2, revision = revision + 1 WHERE id = ?1", params![session, expiry])?;
            transaction.commit()?;
            leases.insert(session, expiry);
            Ok(())
        }).await
    }

    pub async fn renew_turn(&self, session_id: &str) -> Result<()> {
        let session = session_id.to_string();
        self.database.run(move |connection, leases| {
            let expected = *leases.get(&session).ok_or_else(|| eyre!("Session is not owned by this runtime"))?;
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let expiry = now_nanos()?.checked_add(i64::try_from(TURN_LEASE_DURATION.as_nanos())?).ok_or_else(|| eyre!("Lease expiry overflow"))?;
            ensure!(expiry > expected, "Clock moved backwards while renewing session lease");
            let updated = transaction.execute("UPDATE sessions SET lease_expires_at = ?3 WHERE id = ?1 AND lease_active = 1 AND lease_expires_at = ?2", params![session, expected, expiry])?;
            ensure!(updated == 1, kraai_types::DomainError::conflict(format!("Ownership of session {session} was lost")));
            transaction.commit()?;
            leases.insert(session, expiry);
            Ok(())
        }).await
    }

    pub async fn release_turn(&self, session_id: &str) -> Result<bool> {
        let session = session_id.to_string();
        self.database.run(move |connection, leases| {
            let Some(expected) = leases.remove(&session) else { return Ok(false); };
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let updated = transaction.execute("UPDATE sessions SET lease_active = 0, revision = revision + 1 WHERE id = ?1 AND lease_active = 1 AND lease_expires_at = ?2", params![session, expected])?;
            transaction.commit()?;
            Ok(updated == 1)
        }).await
    }

    pub async fn owned_sessions(&self) -> Result<Vec<String>> {
        self.database
            .run(|_, leases| Ok(leases.keys().cloned().collect()))
            .await
    }

    pub async fn owned_turns(&self) -> Result<Vec<(String, i64)>> {
        self.database
            .run(|_, leases| {
                Ok(leases
                    .iter()
                    .map(|(session, expiry)| (session.clone(), *expiry))
                    .collect())
            })
            .await
    }

    pub async fn lease_token_matches(&self, session_id: &str, expected: i64) -> Result<bool> {
        let session = session_id.to_string();
        self.database
            .run(move |_, leases| Ok(leases.get(&session) == Some(&expected)))
            .await
    }
}
