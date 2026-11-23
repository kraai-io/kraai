use std::io;
use std::path::Path;
use std::sync::Arc;

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::ProviderRegistry;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    Browser,
    DeviceCode,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBrowserLogin {
    pub auth_url: String,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDeviceCodeLogin {
    pub verification_url: String,
    pub user_code: String,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderLoginState {
    SignedOut,
    BrowserPending(PendingBrowserLogin),
    DeviceCodePending(PendingDeviceCodeLogin),
    Authenticated,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, Eq, Serialize, Deserialize)]
pub struct ProviderAuthStatus {
    pub provider_type: String,
    /// Orders updates from one provider type; a status with a lower sequence is stale.
    pub sequence: u64,
    pub state: ProviderLoginState,
    pub account: Option<String>,
    pub plan: Option<String>,
    pub last_refresh_unix: Option<u64>,
    pub error: Option<String>,
}

impl PartialEq for ProviderAuthStatus {
    fn eq(&self, other: &Self) -> bool {
        self.provider_type == other.provider_type
            && self.state == other.state
            && self.account == other.account
            && self.plan == other.plan
            && self.last_refresh_unix == other.last_refresh_unix
            && self.error == other.error
    }
}

#[async_trait::async_trait]
pub trait ProviderAuth: Send + Sync {
    fn provider_type(&self) -> &'static str;
    fn login_methods(&self) -> &'static [LoginMethod];
    async fn status(&self) -> ProviderAuthStatus;
    async fn start_login(&self, method: LoginMethod) -> io::Result<ProviderAuthStatus>;
    async fn cancel_login(&self) -> io::Result<ProviderAuthStatus>;
    async fn logout(&self) -> io::Result<ProviderAuthStatus>;
    fn subscribe(&self) -> broadcast::Receiver<ProviderAuthStatus>;
}

#[derive(Default)]
pub struct ProviderSetup {
    pub registry: ProviderRegistry,
    pub auth: Vec<Arc<dyn ProviderAuth>>,
}

#[async_trait::async_trait]
pub trait ProviderSet: Send + Sync {
    async fn load(&self, storage_root: &Path) -> Result<ProviderSetup>;
}

#[cfg(test)]
mod tests;
