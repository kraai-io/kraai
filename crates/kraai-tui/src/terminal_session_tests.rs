use std::process::Command;

use super::{TerminalSessionGuard, terminal_cursor::CursorStyle};

#[test]
fn restores_saved_cursor_on_normal_exit_error_and_panic() {
    const MODE: &str = "KRAAI_CURSOR_CLEANUP_TEST";
    const STYLE: &str = "KRAAI_CURSOR_CLEANUP_STYLE";
    if let Ok(mode) = std::env::var(MODE) {
        let result = (|| -> std::io::Result<()> {
            let mut guard = TerminalSessionGuard::new();
            let style = std::env::var(STYLE).ok().and_then(|code| code.parse().ok());
            guard
                .cursor
                .apply(style.and_then(CursorStyle::from_report))?;
            if mode == "panic" {
                guard.install_panic_hook();
                panic!("intentional cursor cleanup test");
            }
            if mode == "error" {
                return Err(std::io::Error::other("intentional error"));
            }
            guard.restore()
        })();
        assert_eq!(result.is_err(), mode == "error");
        return;
    }

    for (mode, code) in ["normal", "error", "panic"]
        .into_iter()
        .flat_map(|mode| [0, 5].map(|code| (mode, code)))
    {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal_session_tests::restores_saved_cursor_on_normal_exit_error_and_panic",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env(STYLE, code.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.success(), mode != "panic");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some((_, after_override)) = stdout.split_once("\x1b[6 q") else {
            panic!("{mode}: missing steady bar override: {stdout}");
        };
        assert!(
            after_override.contains(&format!("\x1b[{code} q")),
            "{mode}, style {code}: {stdout}"
        );
        let other_code = if code == 0 { 5 } else { 0 };
        assert!(!stdout.contains(&format!("\x1b[{other_code} q")));
        assert!(!stdout.contains("\x1b[2 q"));
    }
}
