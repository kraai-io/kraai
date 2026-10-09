use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Result, bail};

use crate::report::Metadata;

pub fn collect(work_dir: &Path, warmups: u32) -> Result<Metadata> {
    if !cfg!(any(target_os = "linux", target_os = "macos")) {
        bail!("performance measurements currently require Linux or macOS");
    }
    let mut host = BTreeMap::from([
        ("os".into(), std::env::consts::OS.into()),
        ("arch".into(), std::env::consts::ARCH.into()),
        ("kernel".into(), required_command("uname", &["-sr"])?),
        ("hostname".into(), required_command("uname", &["-n"])?),
        (
            "logical_cpus".into(),
            std::thread::available_parallelism()?.to_string(),
        ),
        ("work_dir".into(), work_dir.to_string_lossy().into_owned()),
    ]);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;
        host.insert(
            "work_dir_device".into(),
            fs::metadata(work_dir)?.dev().to_string(),
        );
        let filesystem = nix::sys::statfs::statfs(work_dir)?;
        #[cfg(target_os = "linux")]
        host.insert(
            "work_dir_filesystem".into(),
            format!("{:?}", filesystem.filesystem_type()),
        );
        #[cfg(target_os = "macos")]
        host.insert(
            "work_dir_filesystem".into(),
            filesystem.filesystem_type_name().into(),
        );
    }
    if cfg!(target_os = "linux") {
        let cpu = fs::read_to_string("/proc/cpuinfo")?;
        let model = cpu.lines().find_map(|line| {
            line.strip_prefix("model name")
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim())
        });
        host.insert("cpu".into(), model.unwrap_or(&cpu).to_owned());
        let status = fs::read_to_string("/proc/self/status")?;
        if let Some(affinity) = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        {
            host.insert("cpu_affinity".into(), affinity.trim().into());
        }
        if let Ok(governor) =
            fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        {
            host.insert("cpu_governor".into(), governor.trim().into());
        }
    } else {
        host.insert(
            "cpu".into(),
            required_command("sysctl", &["-n", "machdep.cpu.brand_string"])?,
        );
    }
    let build = BTreeMap::from([
        ("rustc".into(), env!("KRAAI_PERF_RUSTC").into()),
        ("profile".into(), env!("KRAAI_PERF_PROFILE").into()),
        ("opt_level".into(), env!("KRAAI_PERF_OPT_LEVEL").into()),
        ("debug".into(), env!("KRAAI_PERF_DEBUG").into()),
        ("target".into(), env!("KRAAI_PERF_TARGET").into()),
        (
            "target_features".into(),
            env!("KRAAI_PERF_CARGO_CFG_TARGET_FEATURE").into(),
        ),
        (
            "declared_profile_settings".into(),
            env!("KRAAI_PERF_PROFILE_SETTINGS").into(),
        ),
        (
            "profile_and_feature_overrides".into(),
            env!("KRAAI_PERF_BUILD_OVERRIDES").into(),
        ),
        (
            "rustflags".into(),
            env!("KRAAI_PERF_CARGO_ENCODED_RUSTFLAGS").into(),
        ),
    ]);
    let mut provenance = BTreeMap::from([
        (
            "workspace_profiles".into(),
            env!("KRAAI_PERF_PROFILES").into(),
        ),
        (
            "selected_profile".into(),
            env!("KRAAI_PERF_SELECTED_PROFILE").into(),
        ),
        (
            "profile_and_feature_overrides".into(),
            env!("KRAAI_PERF_ALL_BUILD_OVERRIDES").into(),
        ),
        (
            "binary_sha256".into(),
            crate::fingerprint::file(&std::env::current_exe()?)?,
        ),
        (
            "timestamp_unix_seconds".into(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)?
                .as_secs()
                .to_string(),
        ),
    ]);
    if let Some(revision) = command_output("git", &["rev-parse", "HEAD"]) {
        provenance.insert("checkout_revision".into(), revision);
        if let Some(status) = command_output("git", &["status", "--porcelain"]) {
            provenance.insert("checkout_dirty".into(), (!status.is_empty()).to_string());
        }
    }
    Ok(Metadata {
        measurement_schema: measurement_schema(),
        host,
        build,
        provenance,
        warmups,
    })
}

fn measurement_schema() -> String {
    let fingerprint = crate::fingerprint::sources(&[
        include_bytes!("measurement.rs"),
        include_bytes!("benchmark.rs"),
        include_bytes!("runner.rs"),
        include_bytes!("process.rs"),
    ]);
    format!(
        "v2: wall and CPU cover only the benchmark run phase, excluding setup, verification and fixture cleanup; CPU includes children reaped during run; peak_rss_bytes is the worker lifetime peak through the end of run, including setup; children_peak_rss_bytes is the largest reaped child lifetime peak through the end of run, including setup, not concurrent process-tree memory; instrumentation_sha256={fingerprint}"
    )
}

fn required_command(program: &str, args: &[&str]) -> Result<String> {
    command_output(program, args)
        .ok_or_else(|| color_eyre::eyre::eyre!("could not read host metadata using {program}"))
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
