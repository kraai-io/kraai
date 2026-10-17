use std::io;

use crossbeam_channel::Receiver;

#[cfg(unix)]
pub(crate) fn watch() -> io::Result<Receiver<()>> {
    use signal_hook::{consts::SIGHUP, iterator::Signals};

    let mut signals = Signals::new([SIGHUP])?;
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name(String::from("kraai-hangup"))
        .spawn(move || {
            for _ in signals.forever() {
                detach_if_lost();
                let _ = tx.try_send(());
            }
        })?;
    Ok(rx)
}

#[cfg(not(unix))]
pub(crate) fn watch() -> io::Result<Receiver<()>> {
    Ok(crossbeam_channel::never())
}

/// Once the terminal hangs up, points stdout and stderr at /dev/null so that
/// cleanup and panic reporting cannot fail writing to it.
#[cfg(unix)]
pub(crate) fn detach_if_lost() -> bool {
    use std::io::IsTerminal;

    if io::stdout().is_terminal() {
        return false;
    }
    if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
        let _ = rustix::stdio::dup2_stdout(&null);
        let _ = rustix::stdio::dup2_stderr(&null);
    }
    true
}

#[cfg(not(unix))]
pub(crate) fn detach_if_lost() -> bool {
    false
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::process::Command;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the child process asserts on its own hung-up terminal"
    )]
    fn hung_up_terminal_is_detached() -> Result<(), Box<dyn std::error::Error>> {
        const CHILD_ENV: &str = "KRAAI_TERMINAL_LOSS_TEST";
        if std::env::var_os(CHILD_ENV).is_some() {
            use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

            let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
            grantpt(&controller)?;
            unlockpt(&controller)?;
            let path = ptsname(&controller, Vec::new())?;
            let user = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path.to_str()?)?;
            rustix::stdio::dup2_stdout(&user)?;
            assert!(!super::detach_if_lost());
            drop(controller);
            assert!(super::detach_if_lost());
            println!("written after hangup");
            return Ok(());
        }
        let output = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "terminal_loss::tests::hung_up_terminal_is_detached",
            ])
            .env(CHILD_ENV, "1")
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}
