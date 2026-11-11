use super::*;

pub(super) const CONNECT_LABEL: &str = "Connect a provider";

/// What a provider's sheet offers, depending on where its sign-in stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProviderAction {
    SignInBrowser,
    SignInDeviceCode,
    OpenBrowser,
    CopySignIn,
    CancelSignIn,
    LogOut,
    RefreshModels,
    EditConfig,
    Remove,
}

impl ProviderAction {
    pub(super) fn label(self, auth: Option<ProviderAuthState>) -> &'static str {
        let again = auth == Some(ProviderAuthState::Authenticated);
        match self {
            Self::SignInBrowser if again => "Sign in again with the browser",
            Self::SignInBrowser => "Sign in with the browser",
            Self::SignInDeviceCode if again => "Sign in again with a device code",
            Self::SignInDeviceCode => "Sign in with a device code",
            Self::OpenBrowser => "Open the browser again",
            Self::CopySignIn if auth == Some(ProviderAuthState::DeviceCodePending) => {
                "Copy the device code"
            }
            Self::CopySignIn => "Copy the sign-in URL",
            Self::CancelSignIn => "Cancel sign-in",
            Self::LogOut => "Log out",
            Self::RefreshModels => "Refresh models",
            Self::EditConfig => "Edit configuration",
            Self::Remove => "Remove provider",
        }
    }

    fn offered(auth: Option<ProviderAuthState>) -> &'static [Self] {
        match auth {
            None => &[Self::RefreshModels, Self::EditConfig, Self::Remove],
            Some(ProviderAuthState::SignedOut) => &[
                Self::SignInBrowser,
                Self::SignInDeviceCode,
                Self::RefreshModels,
                Self::EditConfig,
                Self::Remove,
            ],
            Some(ProviderAuthState::BrowserPending | ProviderAuthState::DeviceCodePending) => &[
                Self::OpenBrowser,
                Self::CopySignIn,
                Self::CancelSignIn,
                Self::RefreshModels,
                Self::EditConfig,
                Self::Remove,
            ],
            Some(ProviderAuthState::Authenticated) => &[
                Self::RefreshModels,
                Self::EditConfig,
                Self::SignInBrowser,
                Self::SignInDeviceCode,
                Self::LogOut,
                Self::Remove,
            ],
        }
    }
}

impl AppState {
    /// The provider whose sheet or configuration is open.
    pub(super) fn open_provider(&self) -> Option<&ProviderSettings> {
        self.settings_draft
            .as_ref()?
            .providers
            .get(self.settings_provider_index)
    }

    pub(super) fn provider_definition(&self, type_id: &str) -> Option<&ProviderDefinition> {
        self.provider_definitions
            .iter()
            .find(|definition| definition.type_id == type_id)
    }

    /// Where the open provider's sign-in stands, when its type signs in with an account.
    pub(super) fn open_provider_auth(&self) -> Option<&ProviderAuthStatus> {
        self.provider_auth.get(&self.open_provider()?.type_id)
    }

    /// The configured providers the provider list shows, each with its place in the settings.
    pub(super) fn listed_providers(&self) -> Vec<(usize, &ProviderSettings)> {
        let query = self.sheet_query.to_lowercase();
        self.settings_draft
            .iter()
            .flat_map(|draft| draft.providers.iter().enumerate())
            .filter(|(_, provider)| {
                let name = self
                    .provider_definition(&provider.type_id)
                    .map_or("", |definition| definition.display_name.as_str());
                format!("{} {} {name}", provider.id, provider.type_id)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    /// The provider list ends with a row that connects another provider.
    pub(super) fn offers_connect(&self) -> bool {
        self.settings_draft.is_some()
            && CONNECT_LABEL
                .to_lowercase()
                .contains(&self.sheet_query.to_lowercase())
    }

    /// The kinds of provider the connect list shows, OpenAI first.
    pub(super) fn listed_provider_definitions(&self) -> Vec<&ProviderDefinition> {
        let query = self.sheet_query.trim().to_lowercase();
        let mut definitions: Vec<_> = self
            .provider_definitions
            .iter()
            .filter(|definition| {
                format!(
                    "{} {} {}",
                    definition.display_name, definition.type_id, definition.description
                )
                .to_lowercase()
                .contains(&query)
            })
            .collect();
        definitions.sort_by_key(|definition| provider_definition_rank(definition));
        definitions
    }

    /// The actions the open provider offers that the typed text names.
    pub(super) fn listed_provider_actions(&self) -> Vec<ProviderAction> {
        if self.open_provider().is_none() {
            return Vec::new();
        }
        let auth = self.open_provider_auth().map(|auth| auth.state);
        let query = self.sheet_query.to_lowercase();
        ProviderAction::offered(auth)
            .iter()
            .copied()
            .filter(|action| action.label(auth).to_lowercase().contains(&query))
            .collect()
    }

    /// The breadcrumb the composer shows while a provider sheet is open.
    pub(super) fn providers_prefix(&self) -> String {
        let id = self
            .open_provider()
            .map_or("", |provider| provider.id.as_str());
        match (self.providers_view, &self.config_editing) {
            (ProvidersView::List, _) => String::from("/providers "),
            (ProvidersView::Connect, _) => String::from("/providers › connect › "),
            (ProvidersView::Provider, _) => format!("/providers › {id} › "),
            (ProvidersView::Config, None) => format!("/providers › {id} › config › "),
            (ProvidersView::Config, Some(row)) => {
                format!("/providers › {id} › config › {} = ", self.config_label(row))
            }
        }
    }
}

impl App {
    pub(super) fn open_providers_sheet(&mut self) {
        self.show_providers_view(ProvidersView::List);
        self.request(RuntimeRequest::ListProviderDefinitions);
        self.request(RuntimeRequest::GetSettings);
        self.request(RuntimeRequest::GetProviderAuthStatuses);
    }

    pub(super) fn show_providers_view(&mut self, view: ProvidersView) {
        self.open_sheet(UiMode::Providers);
        self.state.providers_view = view;
    }

    /// Stops an edit, or steps out one level: configuration, provider, list, chat.
    pub(super) fn close_providers_level(&mut self) {
        if self.state.config_editing.take().is_some() {
            self.state.sheet_query.clear();
            self.state.sheet_notice = None;
            return;
        }
        match self.state.providers_view {
            ProvidersView::List => self.close_sheet(),
            ProvidersView::Connect => {
                self.show_providers_view(ProvidersView::List);
                self.state.sheet_index = self.state.listed_providers().len();
            }
            ProvidersView::Provider => self.return_to_provider_list(),
            ProvidersView::Config => self.return_to_provider(ProviderAction::EditConfig),
        }
    }

    fn return_to_provider_list(&mut self) {
        let open = self.state.settings_provider_index;
        self.show_providers_view(ProvidersView::List);
        self.state.sheet_index = self
            .state
            .listed_providers()
            .iter()
            .position(|(index, _)| *index == open)
            .unwrap_or(0);
    }

    /// Shows the open provider's actions with `action` highlighted.
    pub(super) fn return_to_provider(&mut self, action: ProviderAction) {
        self.show_providers_view(ProvidersView::Provider);
        self.state.sheet_index = self
            .state
            .listed_provider_actions()
            .iter()
            .position(|listed| *listed == action)
            .unwrap_or(0);
    }

    pub(super) fn handle_providers_key_event(&mut self, key: KeyEvent) {
        self.state.sheet_notice = None;
        self.state.sheet_feedback = None;
        match self.state.providers_view {
            ProvidersView::List => self.handle_provider_list_key(key),
            ProvidersView::Connect => self.handle_connect_key(key),
            ProvidersView::Provider => self.handle_provider_key(key),
            ProvidersView::Config => self.handle_provider_config_key(key),
        }
    }

    fn handle_provider_list_key(&mut self, key: KeyEvent) {
        let armed = std::mem::take(&mut self.state.sheet_armed);
        if key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
            self.refresh_provider_models();
            return;
        }
        let providers: Vec<usize> = self
            .state
            .listed_providers()
            .iter()
            .map(|(index, _)| *index)
            .collect();
        let rows = providers.len() + usize::from(self.state.offers_connect());
        if self.filter_sheet(key, "/providers") || self.walk_sheet(key, rows) {
            return;
        }
        let highlighted = self.state.sheet_index.min(rows.saturating_sub(1));
        match (key.code, providers.get(highlighted).copied()) {
            (KeyCode::Enter, Some(provider)) => {
                self.state.settings_provider_index = provider;
                self.show_providers_view(ProvidersView::Provider);
                self.request(RuntimeRequest::GetProviderAuthStatuses);
            }
            (KeyCode::Enter, None) if rows > 0 => self.show_providers_view(ProvidersView::Connect),
            (KeyCode::Delete, Some(provider)) if armed => self.remove_provider(provider),
            (KeyCode::Delete, Some(_)) => self.state.sheet_armed = true,
            _ => {}
        }
    }

    fn handle_connect_key(&mut self, key: KeyEvent) {
        if self.state.wants_back(key) {
            self.close_providers_level();
            return;
        }
        let len = self.state.listed_provider_definitions().len();
        if self.edit_sheet_filter(key) || self.walk_sheet(key, len) {
            return;
        }
        if key.code == KeyCode::Enter {
            self.connect_highlighted_provider();
        }
    }

    fn handle_provider_key(&mut self, key: KeyEvent) {
        let armed = std::mem::take(&mut self.state.sheet_armed);
        if self.state.wants_back(key) {
            self.return_to_provider_list();
            return;
        }
        let actions = self.state.listed_provider_actions();
        if self.edit_sheet_filter(key) || self.walk_sheet(key, actions.len()) {
            return;
        }
        let highlighted = self.state.sheet_index.min(actions.len().saturating_sub(1));
        if key.code == KeyCode::Enter
            && let Some(action) = actions.get(highlighted).copied()
        {
            self.run_provider_action(action, armed);
        }
    }

    fn run_provider_action(&mut self, action: ProviderAction, armed: bool) {
        let Some(provider_type) = self
            .state
            .open_provider()
            .map(|provider| provider.type_id.clone())
        else {
            return;
        };
        let (request, feedback) = match action {
            ProviderAction::SignInBrowser => (
                RuntimeRequest::StartProviderLogin {
                    provider_type,
                    method: kraai_runtime::LoginMethod::Browser,
                },
                "Starting browser sign-in",
            ),
            ProviderAction::SignInDeviceCode => (
                RuntimeRequest::StartProviderLogin {
                    provider_type,
                    method: kraai_runtime::LoginMethod::DeviceCode,
                },
                "Starting device code sign-in",
            ),
            ProviderAction::CancelSignIn => (
                RuntimeRequest::CancelProviderLogin { provider_type },
                "Cancelling sign-in",
            ),
            ProviderAction::LogOut => (
                RuntimeRequest::LogoutProvider { provider_type },
                "Logging out",
            ),
            ProviderAction::RefreshModels => return self.refresh_provider_models(),
            ProviderAction::OpenBrowser => return self.open_pending_sign_in(),
            ProviderAction::CopySignIn => return self.copy_pending_sign_in(),
            ProviderAction::EditConfig => return self.show_providers_view(ProvidersView::Config),
            ProviderAction::Remove if armed => {
                return self.remove_provider(self.state.settings_provider_index);
            }
            ProviderAction::Remove => {
                self.state.sheet_armed = true;
                return;
            }
        };
        if self.request(request) == RuntimeRequestDelivery::Delivered {
            self.provider_feedback(feedback);
        }
    }

    /// Says what a provider action did, in the provider sheet while one is open.
    pub(super) fn provider_feedback(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.state.mode == UiMode::Providers {
            self.state.sheet_feedback = Some(text.clone());
        }
        self.state.status = text;
    }

    fn refresh_provider_models(&mut self) {
        if self.request(RuntimeRequest::ListModels) == RuntimeRequestDelivery::Delivered {
            self.provider_feedback("Refreshing models");
        }
    }

    fn remove_provider(&mut self, index: usize) {
        let Some(draft) = self.state.settings_draft.as_mut() else {
            return;
        };
        if index >= draft.providers.len() {
            return;
        }
        let provider = draft.providers.remove(index);
        draft
            .models
            .retain(|model| model.provider_id != provider.id);
        self.state.settings_provider_index = 0;
        self.show_providers_view(ProvidersView::List);
        self.state.sheet_index = index;
        self.provider_feedback(format!("Removed {}", provider.id));
        self.save_settings_draft();
    }

    fn connect_highlighted_provider(&mut self) {
        let definitions = self.state.listed_provider_definitions();
        let highlighted = self
            .state
            .sheet_index
            .min(definitions.len().saturating_sub(1));
        let Some((type_id, fields, prefix)) = definitions.get(highlighted).map(|definition| {
            (
                definition.type_id.clone(),
                definition.provider_fields.clone(),
                definition.default_provider_id_prefix.clone(),
            )
        }) else {
            return;
        };
        let Some(draft) = self.state.settings_draft.as_mut() else {
            self.state.sheet_notice = Some(String::from("Provider settings are still loading"));
            return;
        };
        let id = next_provider_id(&draft.providers, &prefix);
        draft.providers.push(ProviderSettings {
            id: id.clone(),
            type_id,
            values: default_values(&fields),
        });
        self.state.settings_provider_index = draft.providers.len().saturating_sub(1);
        self.show_providers_view(ProvidersView::Provider);
        self.provider_feedback(format!("Connected {id}"));
        self.save_settings_draft();
        self.request(RuntimeRequest::GetProviderAuthStatuses);
    }

    pub(super) fn save_settings_draft(&mut self) {
        if let Some(settings) = self.state.settings_draft.clone() {
            self.request(RuntimeRequest::SaveSettings { settings });
        }
    }

    pub(super) fn apply_provider_auth_status(&mut self, status: ProviderAuthStatus) -> bool {
        let previous = self.state.provider_auth.get(&status.provider_type);
        if previous.is_some_and(|previous| status.sequence < previous.sequence) {
            return false;
        }
        let previous_target = previous.and_then(pending_auth_target).map(str::to_owned);
        let next_target = pending_auth_target(&status).map(str::to_owned);
        let device_code = status.state == ProviderAuthState::DeviceCodePending;
        self.state
            .provider_auth
            .insert(status.provider_type.clone(), status);

        if let Some(target) = next_target
            && previous_target.as_deref() != Some(target.as_str())
        {
            self.provider_feedback(match (open_external_target(&target), device_code) {
                (Ok(()), false) => String::from("Opened the browser for sign-in"),
                (Ok(()), true) => {
                    String::from("Opened the browser. Enter the device code there")
                }
                (Err(error), false) => {
                    format!("Could not open the browser: {error}. Copy the sign-in URL instead")
                }
                (Err(error), true) => format!(
                    "Could not open the browser: {error}. Open the URL yourself and enter the device code"
                ),
            });
        }
        true
    }

    fn open_pending_sign_in(&mut self) {
        let target = self
            .state
            .open_provider_auth()
            .and_then(pending_auth_target)
            .map(str::to_owned);
        let Some(target) = target else {
            self.state.sheet_notice = Some(String::from("No sign-in is waiting"));
            return;
        };
        match open_external_target(&target) {
            Ok(()) => self.provider_feedback("Opened the browser"),
            Err(error) => {
                self.state.sheet_notice = Some(format!("Could not open the browser: {error}"));
            }
        }
    }

    fn copy_pending_sign_in(&mut self) {
        let value = self
            .state
            .open_provider_auth()
            .and_then(|status| match status.state {
                ProviderAuthState::BrowserPending => status.auth_url.clone(),
                ProviderAuthState::DeviceCodePending => status.user_code.clone(),
                ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => None,
            });
        let Some(value) = value else {
            self.state.sheet_notice = Some(String::from("No sign-in is waiting"));
            return;
        };
        match self.copy_text_to_clipboard(&value, super::feedback::CopyTarget::Auth) {
            Ok(()) => self.provider_feedback("Copied"),
            Err(error) => self.state.sheet_notice = Some(format!("Copy failed: {error}")),
        }
    }
}
