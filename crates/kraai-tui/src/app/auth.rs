use color_eyre::eyre::Result;
use kraai_runtime::{
    OpenAiCodexAuthStatus as RuntimeOpenAiCodexAuthStatus,
    OpenAiCodexLoginState as RuntimeOpenAiCodexLoginState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum ProviderAuthState {
    #[default]
    SignedOut,
    BrowserPending,
    DeviceCodePending,
    Authenticated,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ProviderAuthStatus {
    pub(super) state: ProviderAuthState,
    pub(super) email: Option<String>,
    pub(super) plan_type: Option<String>,
    pub(super) account_id: Option<String>,
    pub(super) last_refresh: Option<String>,
    pub(super) auth_url: Option<String>,
    pub(super) verification_url: Option<String>,
    pub(super) user_code: Option<String>,
    pub(super) error: Option<String>,
}

pub(super) fn map_openai_codex_auth_status(
    status: RuntimeOpenAiCodexAuthStatus,
) -> ProviderAuthStatus {
    let mut mapped = ProviderAuthStatus {
        state: ProviderAuthState::SignedOut,
        email: status.email,
        plan_type: status.plan_type,
        account_id: status.account_id,
        last_refresh: status.last_refresh_unix.map(|value| value.to_string()),
        auth_url: None,
        verification_url: None,
        user_code: None,
        error: status.error,
    };

    mapped.state = match status.state {
        RuntimeOpenAiCodexLoginState::SignedOut => ProviderAuthState::SignedOut,
        RuntimeOpenAiCodexLoginState::BrowserPending(pending) => {
            mapped.auth_url = Some(pending.auth_url);
            ProviderAuthState::BrowserPending
        }
        RuntimeOpenAiCodexLoginState::DeviceCodePending(pending) => {
            mapped.verification_url = Some(pending.verification_url);
            mapped.user_code = Some(pending.user_code);
            ProviderAuthState::DeviceCodePending
        }
        RuntimeOpenAiCodexLoginState::Authenticated => ProviderAuthState::Authenticated,
    };

    mapped
}

pub(super) fn pending_auth_target(status: &ProviderAuthStatus) -> Option<&str> {
    match status.state {
        ProviderAuthState::BrowserPending => status.auth_url.as_deref(),
        ProviderAuthState::DeviceCodePending => status.verification_url.as_deref(),
        ProviderAuthState::SignedOut | ProviderAuthState::Authenticated => None,
    }
}

pub(super) fn open_external_target(target: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![target]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![target]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", "", target]);

    std::process::Command::new(command.0)
        .args(command.1)
        .spawn()
        .map_err(|err| err.to_string())
        .map(|_| ())
}
