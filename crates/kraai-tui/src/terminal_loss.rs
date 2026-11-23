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

/// Cleanup and panic reporting must not fail writing to a terminal that hung up.
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
mod tests;
