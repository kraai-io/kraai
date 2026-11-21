use super::*;

#[derive(Debug, Clone)]
pub enum HistoryChange {
    Upserted {
        session_id: SessionId,
        message: Box<Message>,
    },
    Removed {
        session_id: SessionId,
        message_ids: Vec<MessageId>,
    },
}

pub type HistoryObserver = Arc<dyn Fn(HistoryChange) + Send + Sync>;

impl AgentManager {
    pub fn set_history_observer(&self, observer: HistoryObserver) {
        *write(&self.history_observer) = Some(observer);
    }

    pub(super) fn history_upserted(&self, session_id: &SessionId, message: &Message) {
        self.history_changed(|| HistoryChange::Upserted {
            session_id: session_id.clone(),
            message: Box::new(message.clone()),
        });
    }

    pub(super) fn history_removed(&self, session_id: &SessionId, message_ids: Vec<MessageId>) {
        if message_ids.is_empty() {
            return;
        }
        self.history_changed(|| HistoryChange::Removed {
            session_id: session_id.clone(),
            message_ids,
        });
    }

    fn history_changed(&self, change: impl FnOnce() -> HistoryChange) {
        let observer = read(&self.history_observer).clone();
        if let Some(observer) = observer {
            observer(change());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamedItem {
    Started { index: usize, item: AssistantItem },
    Delta { index: usize, delta: String },
}
