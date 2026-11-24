use std::collections::{HashMap, VecDeque};

use kraai_runtime::Session;
use kraai_types::{MessageContent, SessionId};

use crate::app::types::{PendingMessage, PendingSubmit};

#[derive(Default)]
pub(in crate::app) struct Sessions {
    pub(in crate::app) list: Vec<Session>,
    pub(in crate::app) workspace_dir: String,
    pub(in crate::app) home_dir: String,
    pub(in crate::app) pending_messages: VecDeque<PendingMessage>,
    pub(in crate::app) failed_messages: HashMap<Option<SessionId>, Vec<MessageContent>>,
    pub(in crate::app) pending_submit: Option<PendingSubmit>,
    pub(in crate::app) next_creation_id: u64,
    pub(in crate::app) next_load_id: u64,
    pub(in crate::app) pending_load_id: Option<u64>,
}
