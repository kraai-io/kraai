use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_types::SandboxCapability;

use super::{Fixture, capabilities, successful_run};
use crate::LaunchPlan;

const HOST_MODE: &str = "KRAAI_MACOS_SYSCTL_HOST";
const HOST_SECRET: &str = "KRAAI_MACOS_SYSCTL_SECRET";
const SECRET: &str = "host-environment-must-stay-private";
const TARGET_PID: &str = "KRAAI_MACOS_SYSCTL_TARGET_PID";

#[tokio::test]
async fn sandbox_cannot_read_host_process_arguments_or_environment() {
    let output = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "tests::macos::sysctl::sysctl_host_child",
            "--nocapture",
        ])
        .env(HOST_MODE, "enabled")
        .env(HOST_SECRET, SECRET)
        .kill_on_drop(true)
        .output()
        .await
        .expect("run isolated host process");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn sysctl_host_child() {
    if std::env::var(HOST_MODE).as_deref() != Ok("enabled") {
        return;
    }
    let pid = i32::try_from(std::process::id()).expect("host PID fits sysctl argument");
    let arguments = read_process_arguments(pid).expect("unsandboxed process arguments");
    for expected in [SECRET, "tests::macos::sysctl::sysctl_host_child"] {
        assert!(
            arguments
                .windows(expected.len())
                .any(|bytes| bytes == expected.as_bytes())
        );
    }

    let fixture = Fixture::new("macos-sysctl");
    let executable = std::env::current_exe().expect("test executable");
    let mut plan = LaunchPlan::new(
        executable.clone(),
        fixture.workspace(),
        capabilities([SandboxCapability::WorkspaceRead]),
        Duration::from_secs(10),
    );
    plan.args = [
        "--exact",
        "tests::macos::sysctl::sysctl_probe_child",
        "--nocapture",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    plan.runtime_roots = vec![
        executable
            .parent()
            .expect("test binary parent")
            .to_path_buf(),
    ];
    if Path::new("/nix/store").exists() {
        plan.runtime_roots.push(PathBuf::from("/nix/store"));
    }
    plan.environment
        .insert(TARGET_PID.into(), pid.to_string().into());
    successful_run(plan).await;
}

#[test]
fn sysctl_probe_child() {
    let Ok(pid) = std::env::var(TARGET_PID) else {
        return;
    };
    assert!(std::env::var_os(HOST_SECRET).is_none());
    let error = read_process_arguments(pid.parse().expect("valid host PID"))
        .expect_err("sandbox must deny host process arguments and environment");
    assert!(
        matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES)),
        "expected sandbox permission denial, got {error}"
    );
}

fn read_process_arguments(pid: libc::c_int) -> io::Result<Vec<u8>> {
    read_sysctl(
        &mut [libc::CTL_KERN, libc::KERN_PROCARGS2, pid],
        argument_limit(),
    )
}

fn argument_limit() -> usize {
    let max_args = read_sysctl(
        &mut [libc::CTL_KERN, libc::KERN_ARGMAX],
        std::mem::size_of::<libc::c_int>(),
    )
    .expect("safe kern.argmax sysctl must remain accessible");
    let max_args = libc::c_int::from_ne_bytes(max_args.try_into().expect("integer argument limit"));
    usize::try_from(max_args).expect("positive argument limit")
}

#[expect(
    unsafe_code,
    reason = "native policy test invokes the sysctl interface directly"
)]
fn read_sysctl(name: &mut [libc::c_int], capacity: usize) -> io::Result<Vec<u8>> {
    let name_length = u32::try_from(name.len()).map_err(io::Error::other)?;
    let mut output = vec![0_u8; capacity];
    let mut length = output.len();
    // SAFETY: both buffers remain valid for the call, and their lengths match their allocations.
    let result = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name_length,
            output.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    output.truncate(length);
    Ok(output)
}
