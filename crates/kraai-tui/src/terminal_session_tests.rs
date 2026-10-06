use std::process::Command;

use super::{TerminalSessionGuard, terminal_cursor::CursorStyle};

#[test]
#[expect(
    clippy::panic,
    clippy::panic_in_result_fn,
    reason = "the child process exercises panic cleanup and the parent asserts captured output"
)]
fn restores_saved_cursor_on_normal_exit_error_and_panic() -> Result<(), Box<dyn std::error::Error>>
{
    const MODE: &str = "KRAAI_CURSOR_CLEANUP_TEST";
    if let Ok(mode) = std::env::var(MODE) {
        let result = (|| -> std::io::Result<()> {
            let mut guard = TerminalSessionGuard::new();
            guard.cursor.apply(CursorStyle::from_report(5))?;
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
        return Ok(());
    }

    for mode in ["normal", "error", "panic"] {
        let output = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "terminal_session_tests::restores_saved_cursor_on_normal_exit_error_and_panic",
                "--nocapture",
            ])
            .env(MODE, mode)
            .output()?;
        assert_eq!(output.status.success(), mode != "panic");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some((_, after_override)) = stdout.split_once("\x1b[6 q") else {
            return Err(format!("{mode}: missing steady bar override: {stdout}").into());
        };
        assert!(after_override.contains("\x1b[5 q"), "{mode}: {stdout}");
        assert!(!stdout.contains("\x1b[0 q"));
        assert!(!stdout.contains("\x1b[2 q"));
    }
    Ok(())
}
