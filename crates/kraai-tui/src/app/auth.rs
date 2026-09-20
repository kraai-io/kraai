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
    pub(super) sequence: u64,
    pub(super) state: ProviderAuthState,
    pub(super) plan_type: Option<String>,
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
        sequence: status.sequence,
        state: ProviderAuthState::SignedOut,
        plan_type: status.plan_type,
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

#[cfg(all(test, unix))]
#[expect(
    clippy::panic_in_result_fn,
    reason = "process lifecycle tests propagate fixture errors and assert child completion"
)]
mod tests {
    use super::*;

    const EXIT_MARKER: &str = "KRAAI_BROWSER_HELPER_EXIT_MARKER";

    #[test]
    fn browser_helper_child() -> Result<(), Box<dyn std::error::Error>> {
        let Some(marker) = std::env::var_os(EXIT_MARKER) else {
            return Ok(());
        };
        let marker = std::path::PathBuf::from(marker);
        std::fs::write(
            marker.with_extension("ready"),
            std::process::id().to_string(),
        )?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(marker.exists(), "parent never released browser helper");
        Ok(())
    }

    #[test]
    fn browser_helpers_are_reaped_without_blocking_the_launcher()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("exit");
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args(["--exact", "app::auth::tests::browser_helper_child"])
            .env(EXIT_MARKER, &marker)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let waiter = spawn_external_target(command)?;
        let ready = marker.with_extension("ready");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready.exists(), "browser helper never started");
        assert!(
            !waiter.is_finished(),
            "launcher waited for a running helper"
        );
        std::fs::write(marker, [])?;
        let status = waiter
            .join()
            .map_err(|_panic| std::io::Error::other("browser helper waiter panicked"))??;
        assert!(status.success());
        #[cfg(target_os = "linux")]
        {
            let child_id = std::fs::read_to_string(ready)?.parse::<u32>()?;
            assert!(!std::path::Path::new(&format!("/proc/{child_id}")).exists());
        }
        Ok(())
    }

    #[test]
    fn browser_helper_preserves_process_spawn_errors() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let missing = directory.path().join("missing-browser");
        let expected = std::process::Command::new(&missing)
            .spawn()
            .err()
            .ok_or_else(|| std::io::Error::other("missing browser unexpectedly started"))?;
        let actual = spawn_external_target(std::process::Command::new(missing))
            .err()
            .ok_or_else(|| std::io::Error::other("missing browser unexpectedly started"))?;
        assert_eq!(actual.kind(), expected.kind());
        assert_eq!(actual.to_string(), expected.to_string());
        Ok(())
    }
}
