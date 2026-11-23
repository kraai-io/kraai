use super::*;
use serde_json::json;

#[test]
fn auth_status_preserves_its_wire_shape() {
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
        let serialized = serde_json::to_value(&status).unwrap();
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
            serde_json::from_value::<ProviderAuthStatus>(serialized).unwrap(),
            status
        );
    }
}
