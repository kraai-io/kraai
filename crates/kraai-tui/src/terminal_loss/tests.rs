use std::process::Command;

#[test]
fn hung_up_terminal_is_detached() {
    const CHILD_ENV: &str = "KRAAI_TERMINAL_LOSS_TEST";
    if std::env::var_os(CHILD_ENV).is_some() {
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

        let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&controller).unwrap();
        unlockpt(&controller).unwrap();
        let path = ptsname(&controller, Vec::new()).unwrap();
        let user = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.to_str().unwrap())
            .unwrap();
        rustix::stdio::dup2_stdout(&user).unwrap();
        assert!(!super::detach_if_lost());
        drop(controller);
        assert!(super::detach_if_lost());
        println!("written after hangup");
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "terminal_loss::tests::hung_up_terminal_is_detached",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
