use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, Result, bail, ensure};

pub(crate) struct Interrupt {
    pub flag: Arc<AtomicBool>,
    handlers: Vec<signal_hook::SigId>,
}

impl Interrupt {
    pub fn new() -> Result<Self> {
        let mut interrupt = Self {
            flag: Arc::new(AtomicBool::new(false)),
            handlers: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            interrupt
                .handlers
                .push(signal_hook::flag::register(signal, interrupt.flag.clone())?);
        }
        Ok(interrupt)
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            signal_hook::low_level::unregister(handler);
        }
    }
}

pub(crate) struct IsolatedWorkspace {
    directory: tempfile::TempDir,
    home: PathBuf,
}

impl IsolatedWorkspace {
    pub fn new(work_dir: &Path) -> Result<Self> {
        let directory = tempfile::tempdir_in(fs::canonicalize(work_dir)?)?;
        let home = directory.path().join("home");
        fs::create_dir(&home)?;
        Ok(Self { directory, home })
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .current_dir(self.directory.path())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.home)
            .env("XDG_DATA_HOME", &self.home)
            .env("LC_ALL", "C")
            .env("TERM", "dumb")
            .stdin(Stdio::null());
    }
}

pub(crate) fn run(
    command: &mut Command,
    timeout: Duration,
    interrupted: &AtomicBool,
    stdout: &Path,
    stderr: &Path,
) -> Result<()> {
    ensure!(
        !interrupted.load(Ordering::Relaxed),
        "performance run interrupted"
    );
    ensure!(!timeout.is_zero(), "process timeout must be positive");
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdout(File::create(stdout)?)
        .stderr(File::create(stderr)?);
    let result = (|| {
        let mut worker = Worker::spawn(command).wrap_err_with(|| format!("start {program}"))?;
        worker.wait(timeout, interrupted)
    })();
    match result {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!(
            "process {program} failed ({status}): {}",
            read_error(stderr)?
        ),
        Err(error) => Err(error.wrap_err(format!(
            "run {program}: {}",
            read_error(stderr).unwrap_or_else(|error| format!("could not read stderr: {error}"))
        ))),
    }
}

pub(crate) fn read_error(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(8192).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

struct Worker(Child);

impl Worker {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.spawn().map(Self)
    }

    fn wait(&mut self, timeout: Duration, interrupted: &AtomicBool) -> Result<ExitStatus> {
        let started = Instant::now();
        loop {
            ensure!(
                !interrupted.load(Ordering::Relaxed),
                "performance run interrupted"
            );
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            ensure!(
                started.elapsed() < timeout,
                "process exceeded timeout of {:.3} seconds",
                timeout.as_secs_f64()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.0.id()) {
            use nix::errno::Errno;
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;

            let group = Pid::from_raw(pid);
            let _ = killpg(group, Signal::SIGTERM);
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                let reaped = matches!(self.0.try_wait(), Ok(Some(_)));
                let group_exited = matches!(killpg(group, None), Err(Errno::ESRCH));
                if reaped && group_exited {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = killpg(group, Signal::SIGKILL);
        }
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
