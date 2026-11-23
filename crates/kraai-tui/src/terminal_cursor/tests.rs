use super::*;

#[test]
fn concurrent_restore_waits_for_the_override_write() {
    use std::sync::mpsc;
    use std::time::Duration;

    struct PausedWriter {
        started: Option<mpsc::SyncSender<()>>,
        resume: mpsc::Receiver<()>,
        bytes: Vec<u8>,
    }

    impl Write for PausedWriter {
        fn write(&mut self, bytes: &[u8]) -> Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).map_err(std::io::Error::other)?;
                self.resume
                    .recv_timeout(kraai_test_support::HANG_TIMEOUT)
                    .map_err(std::io::Error::other)?;
            }
            self.bytes.write(bytes)
        }

        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    let cursor = CursorOverride::default();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let (restoring_tx, restoring_rx) = mpsc::sync_channel(1);
    let (restored_tx, restored_rx) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let applying = scope.spawn(|| {
            let mut output = PausedWriter {
                started: Some(started_tx),
                resume: resume_rx,
                bytes: Vec::new(),
            };
            assert!(cursor.apply_with(None, &mut output).is_ok());
            assert_eq!(output.bytes, b"\x1b[6 q");
        });
        started_rx
            .recv_timeout(kraai_test_support::HANG_TIMEOUT)
            .expect("apply started");
        let restoring = scope.spawn(|| {
            restoring_tx.send(()).expect("restore started");
            let mut output = Vec::new();
            assert!(cursor.restore_with(&mut output).is_ok());
            restored_tx.send(()).expect("restore completed");
            assert_eq!(output, b"\x1b[0 q");
        });
        restoring_rx
            .recv_timeout(kraai_test_support::HANG_TIMEOUT)
            .expect("restore worker started");
        let early_restore = restored_rx.recv_timeout(Duration::from_millis(100));
        resume_tx.send(()).expect("resume apply");
        applying.join().expect("apply worker");
        restoring.join().expect("restore worker");
        assert_eq!(early_restore, Err(mpsc::RecvTimeoutError::Timeout));
    });
    let mut output = Vec::new();
    assert!(cursor.restore_with(&mut output).is_ok());
    assert!(output.is_empty());
}

#[test]
fn panic_during_apply_restores_without_deadlocking() {
    const CHILD_ENV: &str = "KRAAI_CURSOR_WRITE_PANIC_TEST";
    if std::env::var_os(CHILD_ENV).is_some() {
        struct Panics;
        impl Write for Panics {
            #[expect(
                clippy::panic_in_result_fn,
                reason = "the child process panics while apply holds the output lock"
            )]
            fn write(&mut self, _: &[u8]) -> Result<usize> {
                panic!("intentional cursor write panic");
            }

            fn flush(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let cursor = std::sync::Arc::new(CursorOverride::default());
        let cleanup = cursor.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = cleanup.restore();
            previous(info);
        }));
        cursor.apply_with(None, &mut Panics).unwrap();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "terminal_cursor::tests::panic_during_apply_restores_without_deadlocking",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + kraai_test_support::HANG_TIMEOUT;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("cursor panic cleanup did not exit");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("\x1b[0 q"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("intentional cursor write panic"));
}

#[test]
fn preserves_every_reported_shape_and_blink_preference() {
    for code in 1..=6 {
        let style = CursorStyle::from_report(code);
        assert_eq!(CursorStyle::custom_blink(style), matches!(code, 3 | 5));
        let cursor = CursorOverride::default();
        let mut output = Vec::new();
        assert!(cursor.apply_with(style, &mut output).is_ok());
        if !matches!(code, 3 | 5) {
            assert!(output.is_empty());
            assert!(cursor.restore_with(&mut output).is_ok());
            assert!(output.is_empty());
            continue;
        }
        assert_eq!(output, format!("\x1b[{} q", code + code % 2).as_bytes());
        output.clear();
        assert!(cursor.apply_with(style, &mut output).is_ok());
        assert!(output.is_empty());
        assert!(cursor.restore_with(&mut output).is_ok());
        assert_eq!(output, format!("\x1b[{code} q").as_bytes());
        output.clear();
        assert!(cursor.restore_with(&mut output).is_ok());
        assert!(output.is_empty());
        assert!(cursor.apply_with(style, &mut output).is_ok());
        assert_eq!(output, format!("\x1b[{} q", code + code % 2).as_bytes());
    }
}

#[test]
fn unknown_or_default_style_uses_bar_and_restores_terminal_default() {
    for code in [0, 7, 255] {
        let cursor = CursorOverride::default();
        let mut output = Vec::new();
        assert!(
            cursor
                .apply_with(CursorStyle::from_report(code), &mut output)
                .is_ok()
        );
        assert_eq!(output, b"\x1b[6 q");
        output.clear();
        assert!(cursor.apply_with(None, &mut output).is_ok());
        assert!(output.is_empty());
        assert!(cursor.restore_with(&mut output).is_ok());
        assert_eq!(output, b"\x1b[0 q");
        output.clear();
        assert!(cursor.restore_with(&mut output).is_ok());
        assert!(output.is_empty());
        assert!(cursor.apply_with(None, &mut output).is_ok());
        assert_eq!(output, b"\x1b[6 q");
    }
}

#[test]
fn late_reports_replace_fallback_and_restore_detected_preferences() {
    for code in 1..=6 {
        let cursor = CursorOverride::default();
        let mut output = Vec::new();
        assert!(cursor.apply_with(None, &mut output).is_ok());
        output.clear();
        let style = CursorStyle::from_report(code);
        assert!(cursor.apply_with(style, &mut output).is_ok());
        let applied = if matches!(code, 3 | 5) {
            code + 1
        } else {
            code
        };
        assert_eq!(output, format!("\x1b[{applied} q").as_bytes());
        output.clear();
        assert!(cursor.apply_with(style, &mut output).is_ok());
        assert!(output.is_empty());
        assert!(cursor.restore_with(&mut output).is_ok());
        if matches!(code, 3 | 5) {
            assert_eq!(output, format!("\x1b[{code} q").as_bytes());
        } else {
            assert!(output.is_empty());
        }
    }
}

#[test]
fn failed_late_report_writes_retain_detected_style_for_cleanup() {
    for code in 1..=6 {
        let cursor = CursorOverride::default();
        assert!(cursor.apply_with(None, &mut Vec::new()).is_ok());
        let mut output = [0; 0];
        assert!(
            cursor
                .apply_with(CursorStyle::from_report(code), &mut output.as_mut_slice())
                .is_err()
        );
        let mut output = Vec::new();
        assert!(cursor.restore_with(&mut output).is_ok());
        assert_eq!(output, format!("\x1b[{code} q").as_bytes());
    }
}

#[test]
fn failed_writes_retain_the_original_style_for_cleanup() {
    struct Fails {
        write: bool,
    }
    impl Write for Fails {
        fn write(&mut self, bytes: &[u8]) -> Result<usize> {
            if self.write {
                Err(std::io::Error::other("write failed"))
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> Result<()> {
            Err(std::io::Error::other("flush failed"))
        }
    }
    for (write, code) in [false, true]
        .into_iter()
        .flat_map(|write| [0, 5].map(|code| (write, code)))
    {
        let cursor = CursorOverride::default();
        let mut failing = Fails { write };
        assert!(
            cursor
                .apply_with(CursorStyle::from_report(code), &mut failing)
                .is_err()
        );
        assert!(cursor.restore_with(&mut failing).is_err());
        let mut output = Vec::new();
        assert!(cursor.restore_with(&mut output).is_ok());
        assert_eq!(output, format!("\x1b[{code} q").as_bytes());
    }
}
