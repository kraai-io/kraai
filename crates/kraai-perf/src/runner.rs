use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use color_eyre::eyre::{Context, Result, ensure};

use crate::benchmark::Workload;
use crate::process::{self, Interrupt, IsolatedWorkspace};
use crate::report::{Sample, WorkloadReport, WorkloadSpec};

pub fn run(
    workloads: &[Workload],
    samples: u32,
    warmups: u32,
    work_dir: &Path,
    timeout_seconds: u32,
) -> Result<Vec<WorkloadReport>> {
    let interrupted = Interrupt::new()?;
    let mut reports = Vec::new();
    for workload in workloads {
        let mut measured = Vec::new();
        for _ in 0..warmups {
            sample(&workload.name, work_dir, timeout_seconds, &interrupted.flag)?;
        }
        for _ in 0..samples {
            measured.push(sample(
                &workload.name,
                work_dir,
                timeout_seconds,
                &interrupted.flag,
            )?);
        }
        reports.push(WorkloadReport::new(
            WorkloadSpec {
                name: workload.name.clone(),
                operations: workload.operations,
                fingerprint: workload.fingerprint.clone(),
                parameters: serde_json::json!({"description": workload.description}),
            },
            measured,
        )?);
    }
    Ok(reports)
}

fn sample(
    name: &str,
    work_dir: &Path,
    timeout_seconds: u32,
    interrupted: &AtomicBool,
) -> Result<Sample> {
    ensure!(
        !interrupted.load(Ordering::Relaxed),
        "performance run interrupted"
    );
    let workspace = IsolatedWorkspace::new(work_dir)?;
    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["sample", name]);
    workspace.configure(&mut command);
    let timeout = Duration::from_secs(u64::from(timeout_seconds));
    process::run(
        &mut command,
        timeout,
        interrupted,
        stdout.path(),
        stderr.path(),
    )
    .wrap_err_with(|| format!("run workload {name}"))?;
    serde_json::from_reader(File::open(stdout.path())?.take(64 * 1024))
        .wrap_err_with(|| format!("read sample for {name}"))
}

#[cfg(all(test, unix))]
#[path = "runner_tests.rs"]
mod tests;
