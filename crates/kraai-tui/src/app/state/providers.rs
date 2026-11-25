use std::collections::{BTreeMap, HashMap};

use kraai_runtime::{McpAuthStatus, ProviderDefinition, SettingsDocument};

use crate::app::auth::ProviderAuthStatus;
use crate::app::mcp_auth::BrowserIntent;
use crate::app::provider_config::ConfigRow;
use crate::app::types::ProvidersView;

pub(in crate::app) struct Providers {
    pub(in crate::app) definitions: Vec<ProviderDefinition>,
    pub(in crate::app) auth: HashMap<String, ProviderAuthStatus>,
    pub(in crate::app) view: ProvidersView,
    pub(in crate::app) config_editing: Option<ConfigRow>,
    pub(in crate::app) settings_draft: Option<SettingsDocument>,
    pub(in crate::app) settings_errors: HashMap<String, String>,
    pub(in crate::app) settings_provider_index: usize,
}

impl Default for Providers {
    fn default() -> Self {
        Self {
            definitions: Vec::new(),
            auth: HashMap::new(),
            view: ProvidersView::List,
            config_editing: None,
            settings_draft: None,
            settings_errors: HashMap::new(),
            settings_provider_index: 0,
        }
    }
}

#[derive(Default)]
pub(in crate::app) struct Mcp {
    pub(in crate::app) auth: BTreeMap<String, McpAuthStatus>,
    pub(in crate::app) browser_intent: BTreeMap<String, BrowserIntent>,
    pub(in crate::app) next_login_request: u64,
    pub(in crate::app) open: Option<String>,
    pub(in crate::app) loaded: bool,
    pub(in crate::app) feedback: BTreeMap<String, String>,
    pub(in crate::app) error: Option<String>,
}
