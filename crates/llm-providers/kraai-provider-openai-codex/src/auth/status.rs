use serde::{Deserialize, Serialize};

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
pub enum OpenAiCodexLoginState {
    SignedOut,
    BrowserPending(PendingBrowserLogin),
    DeviceCodePending(PendingDeviceCodeLogin),
    Authenticated,
}

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCodexAuthStatus {
    pub state: OpenAiCodexLoginState,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub account_id: Option<String>,
    pub last_refresh_unix: Option<u64>,
    pub error: Option<String>,
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
    fn auth_status_preserves_runtime_wire_shape() -> serde_json::Result<()> {
        for (state, expected) in [
            (OpenAiCodexLoginState::SignedOut, json!("SignedOut")),
            (OpenAiCodexLoginState::Authenticated, json!("Authenticated")),
            (
                OpenAiCodexLoginState::BrowserPending(PendingBrowserLogin {
                    auth_url: String::from("https://login.invalid"),
                }),
                json!({"BrowserPending": {"auth_url": "https://login.invalid"}}),
            ),
            (
                OpenAiCodexLoginState::DeviceCodePending(PendingDeviceCodeLogin {
                    verification_url: String::from("https://device.invalid"),
                    user_code: String::from("ABCD"),
                }),
                json!({"DeviceCodePending": {
                    "verification_url": "https://device.invalid", "user_code": "ABCD"
                }}),
            ),
        ] {
            let status = OpenAiCodexAuthStatus {
                state,
                email: Some(String::from("user@example.com")),
                plan_type: None,
                account_id: Some(String::from("account")),
                last_refresh_unix: Some(42),
                error: None,
            };
            let serialized = serde_json::to_value(&status)?;
            let expected = json!({
                "state": expected,
                "email": "user@example.com",
                "plan_type": null,
                "account_id": "account",
                "last_refresh_unix": 42,
                "error": null,
            });
            assert_eq!(serialized, expected);
            assert_eq!(
                serde_json::from_value::<OpenAiCodexAuthStatus>(serialized)?,
                status
            );
        }
        Ok(())
    }
}
