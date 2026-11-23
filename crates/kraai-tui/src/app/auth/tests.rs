use super::*;

const EXIT_MARKER: &str = "KRAAI_BROWSER_HELPER_EXIT_MARKER";

#[test]
fn browser_helper_child() {
    let Some(marker) = std::env::var_os(EXIT_MARKER) else {
        return;
    };
    let marker = std::path::PathBuf::from(marker);
    std::fs::write(
        marker.with_extension("ready"),
        std::process::id().to_string(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + kraai_test_support::HANG_TIMEOUT;
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(marker.exists(), "parent never released browser helper");
}

#[test]
fn browser_helpers_are_reaped_without_blocking_the_launcher() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("exit");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "app::auth::tests::browser_helper_child"])
        .env(EXIT_MARKER, &marker)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let waiter = spawn_external_target(command).unwrap();
    let ready = marker.with_extension("ready");
    let deadline = std::time::Instant::now() + kraai_test_support::HANG_TIMEOUT;
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(ready.exists(), "browser helper never started");
    assert!(
        !waiter.is_finished(),
        "launcher waited for a running helper"
    );
    std::fs::write(marker, []).unwrap();
    let status = waiter
        .join()
        .expect("browser helper waiter panicked")
        .unwrap();
    assert!(status.success());
    #[cfg(target_os = "linux")]
    {
        let child_id = std::fs::read_to_string(ready)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{child_id}")).exists());
    }
}

#[test]
fn browser_helper_preserves_process_spawn_errors() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-browser");
    let expected = std::process::Command::new(&missing)
        .spawn()
        .expect_err("missing browser unexpectedly started");
    let actual = spawn_external_target(std::process::Command::new(missing))
        .expect_err("missing browser unexpectedly started");
    assert_eq!(actual.kind(), expected.kind());
    assert_eq!(actual.to_string(), expected.to_string());
}
