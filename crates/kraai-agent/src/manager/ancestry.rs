use super::*;

impl AgentManager {
    pub async fn reachable_message_ids(
        &self,
        session_id: &str,
        mut candidates: HashSet<MessageId>,
    ) -> Result<HashSet<MessageId>> {
        let mut reachable = HashSet::new();
        if candidates.is_empty() {
            return Ok(reachable);
        }
        let mut current = self.get_tip(session_id).await?;
        let mut visited = HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id.clone()) {
                return Err(eyre!(
                    "Corrupt message parent graph: cycle repeats message {id}"
                ));
            }
            current = self.message_store.read_parent_id(&id).await?;
            if current.is_none() && self.message_store.get(&id).await?.is_none() {
                return Err(eyre!(
                    "Message {id} disappeared while reading session history"
                ));
            }
            if candidates.remove(&id) {
                reachable.insert(id);
                if candidates.is_empty() {
                    break;
                }
            }
        }
        Ok(reachable)
    }
}
