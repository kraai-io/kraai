use super::*;

impl AgentManager {
    pub async fn reachable_message_ids(
        &self,
        session_id: &SessionId,
        mut candidates: HashSet<MessageId>,
    ) -> Result<HashSet<MessageId>> {
        let mut reachable = HashSet::new();
        if candidates.is_empty() {
            return Ok(reachable);
        }
        let mut history =
            HistoryCursor::new(self.message_store.as_ref(), self.get_tip(session_id).await?);
        while !candidates.is_empty()
            && let Some(page) = history.next_page().await?
        {
            for message in page {
                if candidates.remove(&message.id) {
                    reachable.insert(message.id);
                }
            }
        }
        Ok(reachable)
    }
}
