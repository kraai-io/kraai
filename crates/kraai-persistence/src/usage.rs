use crate::database::{Database, assert_owner, bump_revision, list_records, write_record};
use color_eyre::eyre::Result;
use kraai_types::{MessageId, RequestUsage};
use std::collections::BTreeMap;
use std::path::Path;

#[async_trait::async_trait]
pub trait RequestUsageStore: Send + Sync {
    async fn save(&self, session_id: &str, request: &RequestUsage) -> Result<()>;
    async fn delete(&self, session_id: &str) -> Result<()>;
    async fn load(&self, session_id: &str) -> Result<BTreeMap<MessageId, RequestUsage>>;
}

pub struct SqliteRequestUsageStore {
    database: Database,
}
impl SqliteRequestUsageStore {
    pub fn new(data_dir: &Path) -> Self {
        Self::with_database(Database::new(data_dir))
    }
    pub(crate) fn with_database(database: Database) -> Self {
        Self { database }
    }
}

#[async_trait::async_trait]
impl RequestUsageStore for SqliteRequestUsageStore {
    async fn save(&self, session_id: &str, request: &RequestUsage) -> Result<()> {
        let session = session_id.to_string();
        let request = request.clone();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                write_record(
                    transaction,
                    "usage",
                    request.message_id.as_str(),
                    Some(&session),
                    &request,
                )
            })
            .await
    }
    async fn delete(&self, session_id: &str) -> Result<()> {
        let session = session_id.to_string();
        self.database
            .transaction(move |transaction, leases| {
                assert_owner(transaction, leases, &session)?;
                transaction.execute(
                    "DELETE FROM records WHERE kind = 'usage' AND session_id = ?1",
                    [&session],
                )?;
                bump_revision(transaction, &session)
            })
            .await
    }
    async fn load(&self, session_id: &str) -> Result<BTreeMap<MessageId, RequestUsage>> {
        let session = session_id.to_string();
        self.database
            .run(move |connection, _| {
                Ok(
                    list_records::<RequestUsage>(connection, "usage", Some(&session))?
                        .into_iter()
                        .map(|request| (request.message_id.clone(), request))
                        .collect(),
                )
            })
            .await
    }
}
