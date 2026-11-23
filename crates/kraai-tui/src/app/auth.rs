use color_eyre::eyre::Result;
use kraai_runtime::{
    ProviderAuthStatus as RuntimeProviderAuthStatus,
    ProviderLoginState as RuntimeProviderLoginState,
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
    pub(super) provider_type: String,
    pub(super) sequence: u64,
    pub(super) state: ProviderAuthState,
    pub(super) plan_type: Option<String>,
    pub(super) last_refresh: Option<String>,
    pub(super) auth_url: Option<String>,
    pub(super) verification_url: Option<String>,
    pub(super) user_code: Option<String>,
    pub(super) error: Option<String>,
}

pub(super) fn map_provider_auth_status(status: RuntimeProviderAuthStatus) -> ProviderAuthStatus {
    let mut mapped = ProviderAuthStatus {
        provider_type: status.provider_type,
        sequence: status.sequence,
        state: ProviderAuthState::SignedOut,
        plan_type: status.plan,
        last_refresh: status.last_refresh_unix.map(|value| value.to_string()),
        auth_url: None,
        verification_url: None,
        user_code: None,
        error: status.error,
    };

    mapped.state = match status.state {
        RuntimeProviderLoginState::SignedOut => ProviderAuthState::SignedOut,
        RuntimeProviderLoginState::BrowserPending(pending) => {
            mapped.auth_url = Some(pending.auth_url);
            ProviderAuthState::BrowserPending
        }
        RuntimeProviderLoginState::DeviceCodePending(pending) => {
            mapped.verification_url = Some(pending.verification_url);
            mapped.user_code = Some(pending.user_code);
            ProviderAuthState::DeviceCodePending
        }
        RuntimeProviderLoginState::Authenticated => ProviderAuthState::Authenticated,
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

#[cfg(not(target_os = "windows"))]
pub(super) fn open_external_target(target: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![target]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![target]);

    let mut process = std::process::Command::new(command.0);
    process.args(command.1);
    drop(spawn_external_target(process).map_err(|err| err.to_string())?);
    Ok(())
}

#[cfg(target_os = "windows")]
pub(super) fn open_external_target(target: &str) -> Result<(), String> {
    webbrowser::open(target).map_err(|err| err.to_string())
}

#[cfg(not(target_os = "windows"))]
fn spawn_external_target(
    mut command: std::process::Command,
) -> std::io::Result<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>> {
    let (started, receiver) = std::sync::mpsc::sync_channel(0);
    let waiter = std::thread::Builder::new()
        .name(String::from("kraai-browser"))
        .spawn(move || {
            let mut child = kraai_sandbox::spawn_command(&mut command)?;
            let _ = started.send(());
            child.wait()
        })?;
    match receiver.recv() {
        Ok(()) => Ok(waiter),
        Err(disconnected) => match waiter.join() {
            Ok(Err(error)) => Err(error),
            Ok(Ok(_)) => Err(std::io::Error::other(disconnected)),
            Err(_panic) => Err(std::io::Error::other("browser helper worker panicked")),
        },
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
