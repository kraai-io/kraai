//! Interactive sign-in for provider types that authenticate with an account rather than a
//! configured key, so frontends handle every such provider the same way.

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
    /// The provider type this sign-in serves, such as `openai-codex`.
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
    /// Every status change, including those caused by background token refreshes.
    fn subscribe(&self) -> broadcast::Receiver<ProviderAuthStatus>;
}

/// The provider types a runtime can configure and the sign-ins they use.
#[derive(Default)]
pub struct ProviderSetup {
    pub registry: ProviderRegistry,
    pub auth: Vec<Arc<dyn ProviderAuth>>,
}

/// Builds the provider types for a runtime once its storage root is known.
#[async_trait::async_trait]
pub trait ProviderSet: Send + Sync {
    async fn load(&self, storage_root: &Path) -> Result<ProviderSetup>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "wire contract assertions identify serialization regressions"
    )]
    fn auth_status_preserves_its_wire_shape() -> serde_json::Result<()> {
        for (state, expected) in [
            (ProviderLoginState::SignedOut, json!("SignedOut")),
            (ProviderLoginState::Authenticated, json!("Authenticated")),
            (
                ProviderLoginState::BrowserPending(PendingBrowserLogin {
                    auth_url: String::from("https://login.invalid"),
                }),
                json!({"BrowserPending": {"auth_url": "https://login.invalid"}}),
            ),
            (
                ProviderLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                    verification_url: String::from("https://device.invalid"),
                    user_code: String::from("ABCD"),
                }),
                json!({"DeviceCodePending": {
                    "verification_url": "https://device.invalid", "user_code": "ABCD"
                }}),
            ),
        ] {
            let status = ProviderAuthStatus {
                provider_type: String::from("example"),
                sequence: 42,
                state,
                account: Some(String::from("user@example.com")),
                plan: None,
                last_refresh_unix: Some(42),
                error: None,
            };
            let serialized = serde_json::to_value(&status)?;
            let expected = json!({
                "provider_type": "example",
                "sequence": 42,
                "state": expected,
                "account": "user@example.com",
                "plan": null,
                "last_refresh_unix": 42,
                "error": null,
            });
            assert_eq!(serialized, expected);
            let mut later = status.clone();
            later.sequence += 1;
            assert_eq!(status, later);
            assert_eq!(
                serde_json::from_value::<ProviderAuthStatus>(serialized)?,
                status
            );
        }
        Ok(())
    }
}
