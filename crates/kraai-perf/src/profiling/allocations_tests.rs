use std::ffi::OsStr;

use color_eyre::eyre::bail;
use serde_json::{Value, json};

use super::*;

fn profile(pid: u32, command: &str) -> Value {
    json!({
        "dhatFileVersion": 2,
        "mode": "heap",
        "bklt": true,
        "cmd": command,
        "pid": pid,
        "pps": [
            {"tb": 100, "tbk": 2, "mb": 80, "gb": 60, "eb": 20, "fs": [1, 2, 3]},
            {"tb": 50, "tbk": 1, "mb": 50, "gb": 30, "eb": 10, "fs": [1, 2, 3]}
        ],
        "ftbl": [
            "[root]",
            "0x123: malloc (in /lib/valgrind/vgpreload_dhat-amd64-linux.so)",
            "0x456: crate::allocate (source.rs:12)",
            "0x789: crate::run (source.rs:1)"
        ]
    })
}

fn write_profile(directory: &Path, pid: u32, profile: &Value) -> Result<()> {
    fs::write(
        directory.join(format!("dhat.{pid}.json")),
        serde_json::to_vec(profile)?,
    )?;
    fs::write(
        directory.join(format!("valgrind.{pid}.log")),
        "DHAT fixture log",
    )?;
    Ok(())
}

#[test]
fn keeps_process_totals_separate_and_reverses_call_stacks() -> Result<()> {
    let directory = tempfile::tempdir()?;
    write_profile(
        directory.path(),
        11,
        &profile(11, "kraai-perf profile-worker nushell-execution"),
    )?;
    write_profile(
        directory.path(),
        22,
        &profile(
            22,
            "kraai-perf --kraai-internal-nushell-host --transport fixture",
        ),
    )?;
    let mut folded = Vec::new();
    let summaries = summarize(directory.path(), &mut folded)?;
    ensure!(summaries.len() == 2);
    for summary in summaries {
        ensure!(summary.total_allocated_bytes == 150);
        ensure!(summary.total_allocations == 3);
        ensure!(summary.peak_live_bytes == 90);
        ensure!(summary.live_bytes_at_exit == 30);
        ensure!(summary.raw_filename == format!("dhat.{}.json", summary.pid));
    }
    let folded = String::from_utf8(folded)?;
    ensure!(
        folded.contains(
            "Workload worker;crate::run (source.rs:1);crate::allocate (source.rs:12) 100\n"
        )
    );
    ensure!(
        folded
            .contains("Nushell host;crate::run (source.rs:1);crate::allocate (source.rs:12) 50\n")
    );
    ensure!(!folded.contains("malloc"));
    Ok(())
}

#[test]
fn escapes_folded_stack_delimiters_without_losing_symbol_text() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut data = profile(11, "kraai-perf profile-worker fixture");
    *data
        .pointer_mut("/ftbl/2")
        .ok_or_else(|| eyre!("missing test frame"))? =
        json!("0x456: crate::allocate<;>\nnext\rline\\path\t");
    write_profile(directory.path(), 11, &data)?;
    let mut folded = Vec::new();
    summarize(directory.path(), &mut folded)?;
    let folded = String::from_utf8(folded)?;
    ensure!(folded.lines().count() == 2);
    ensure!(folded.contains("crate::allocate<\\x3b>\\nnext\\rline\\\\path\\t"));
    Ok(())
}

#[test]
fn rejects_invalid_profiles_and_counter_overflow() -> Result<()> {
    for (pointer, replacement) in [
        ("/dhatFileVersion", json!(3)),
        ("/mode", json!("copy")),
        ("/bklt", json!(false)),
        ("/pid", json!(0)),
        ("/cmd", json!(" ")),
        ("/ftbl/0", json!("missing root")),
        ("/pps/0/fs", json!([99])),
        ("/pps/0/fs", json!([])),
        ("/pps/0/gb", json!(101)),
        ("/pps/0/eb", json!(101)),
        ("/pps/0/tbk", json!(0)),
        ("/pps/0/tb", json!(u64::MAX)),
        ("/pps/0/tbk", json!(u64::MAX)),
    ] {
        let directory = tempfile::tempdir()?;
        let mut data = profile(11, "kraai-perf profile-worker fixture");
        *data
            .pointer_mut(pointer)
            .ok_or_else(|| eyre!("missing test field {pointer}"))? = replacement;
        write_profile(directory.path(), 11, &data)?;
        ensure!(
            summarize(directory.path(), &mut Vec::new()).is_err(),
            "accepted invalid field {pointer}"
        );
    }
    Ok(())
}

#[test]
fn global_peak_uses_live_snapshot_even_when_point_maximum_is_zero() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut data = profile(11, "kraai-perf profile-worker fixture");
    *data
        .pointer_mut("/pps/0/mb")
        .ok_or_else(|| eyre!("missing test maximum"))? = json!(0);
    write_profile(directory.path(), 11, &data)?;
    let summaries = summarize(directory.path(), &mut Vec::new())?;
    let summary = summaries.first().ok_or_else(|| eyre!("missing summary"))?;
    ensure!(summary.peak_live_bytes == 90);
    ensure!(summary.live_bytes_at_exit == 30);
    Ok(())
}

#[test]
fn rejects_missing_malformed_and_mismatched_process_profiles() -> Result<()> {
    let directory = tempfile::tempdir()?;
    ensure!(summarize(directory.path(), &mut Vec::new()).is_err());
    fs::write(directory.path().join("dhat.11.json"), "unfinished JSON")?;
    fs::write(directory.path().join("valgrind.11.log"), "DHAT fixture log")?;
    ensure!(summarize(directory.path(), &mut Vec::new()).is_err());
    let data = profile(11, "kraai-perf profile-worker fixture");
    write_profile(directory.path(), 11, &data)?;
    write_profile(directory.path(), 22, &data)?;
    let Err(error) = summarize(directory.path(), &mut Vec::new()) else {
        bail!("accepted a profile with the wrong process ID");
    };
    ensure!(
        error
            .to_string()
            .contains("does not match filename process ID 22")
    );
    Ok(())
}

#[test]
fn rejects_missing_child_profile_with_bounded_log_diagnostics() -> Result<()> {
    let directory = tempfile::tempdir()?;
    write_profile(
        directory.path(),
        11,
        &profile(11, "kraai-perf profile-worker nushell-execution"),
    )?;
    let message = "error: can't open DHAT output file for child";
    let log = format!("{message}\n{}diagnostic beyond limit", "x".repeat(16384));
    fs::write(directory.path().join("valgrind.22.log"), log)?;
    let mut folded = Vec::new();
    let Err(error) = summarize(directory.path(), &mut folded) else {
        bail!("accepted a capture missing its child profile");
    };
    let error = error.to_string();
    ensure!(error.contains("Missing DHAT profile for process 22"));
    ensure!(error.contains(message));
    ensure!(!error.contains("diagnostic beyond limit"));
    ensure!(error.len() < 9000);
    ensure!(folded.is_empty());
    Ok(())
}

#[test]
fn rejects_profiles_without_logs_and_duplicate_artifact_ids() -> Result<()> {
    let directory = tempfile::tempdir()?;
    write_profile(
        directory.path(),
        11,
        &profile(11, "kraai-perf profile-worker fixture"),
    )?;
    fs::remove_file(directory.path().join("valgrind.11.log"))?;
    let Err(error) = summarize(directory.path(), &mut Vec::new()) else {
        bail!("accepted a profile without its log");
    };
    ensure!(
        error
            .to_string()
            .contains("Missing Valgrind log for process 11")
    );
    fs::write(directory.path().join("valgrind.11.log"), "DHAT fixture log")?;
    fs::copy(
        directory.path().join("dhat.11.json"),
        directory.path().join("dhat.011.json"),
    )?;
    let Err(error) = summarize(directory.path(), &mut Vec::new()) else {
        bail!("accepted duplicate profile files for one process ID");
    };
    ensure!(
        error
            .to_string()
            .contains("Duplicate Valgrind artifact for process ID 11")
    );
    Ok(())
}

#[test]
fn command_traces_children_with_separate_artifacts_and_inline_frames() -> Result<()> {
    let command = record_command(
        Path::new("/tmp/profile output"),
        Path::new("/tmp/kraai perf"),
        "nushell-execution",
    );
    ensure!(command.get_program() == OsStr::new("valgrind"));
    let args: Vec<_> = command.get_args().collect();
    ensure!(args.contains(&OsStr::new("--trace-children=yes")));
    ensure!(args.contains(&OsStr::new("--read-inline-info=yes")));
    ensure!(args.contains(&OsStr::new("--enable-debuginfod=no")));
    ensure!(args.contains(&OsStr::new(
        "--dhat-out-file=/tmp/profile output/dhat.%p.json"
    )));
    ensure!(args.ends_with(&[
        OsStr::new("/tmp/kraai perf"),
        OsStr::new("profile-worker"),
        OsStr::new("nushell-execution"),
    ]));
    Ok(())
}
