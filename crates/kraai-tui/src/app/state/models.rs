use std::collections::{BTreeMap, HashMap};

use kraai_runtime::{AgentProfileSummary, AgentProfileWarning, Model};
use kraai_types::{ModelOptionValues, ModelSelection, SessionId};

use crate::app::session_model::SessionModelSave;

#[derive(Default)]
pub(in crate::app) struct Models {
    pub(in crate::app) by_provider: HashMap<String, Vec<Model>>,
    pub(in crate::app) selected_provider_id: Option<String>,
    pub(in crate::app) selected_model_id: Option<String>,
    pub(in crate::app) selected_options: ModelOptionValues,
    pub(in crate::app) remembered_options: BTreeMap<String, BTreeMap<String, ModelOptionValues>>,
    pub(in crate::app) last_session_model: Option<ModelSelection>,
    pub(in crate::app) session_saves: HashMap<SessionId, SessionModelSave>,
    pub(in crate::app) next_save_id: u64,
}

#[derive(Default)]
pub(in crate::app) struct Agents {
    pub(in crate::app) profiles: Vec<AgentProfileSummary>,
    pub(in crate::app) warnings: Vec<AgentProfileWarning>,
    pub(in crate::app) selected_profile_id: Option<String>,
    pub(in crate::app) sandbox_warning: Option<String>,
}
