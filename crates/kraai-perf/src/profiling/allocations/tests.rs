use std::ffi::OsStr;

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

fn write_profile(directory: &Path, pid: u32, profile: &Value) {
    fs::write(
        directory.join(format!("dhat.{pid}.json")),
        serde_json::to_vec(profile).unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join(format!("valgrind.{pid}.log")),
        "DHAT fixture log",
    )
    .unwrap();
}

#[test]
fn keeps_process_totals_separate_and_reverses_call_stacks() {
    let directory = tempfile::tempdir().unwrap();
    write_profile(
        directory.path(),
        11,
        &profile(11, "kraai-perf profile-worker nushell-execution"),
    );
    write_profile(
        directory.path(),
        22,
        &profile(
            22,
            "kraai-perf --kraai-internal-nushell-host --transport fixture",
        ),
    );
    let mut folded = Vec::new();
    let summaries = summarize(directory.path(), &mut folded).unwrap();
    assert!(summaries.len() == 2);
    for summary in summaries {
        assert!(summary.total_allocated_bytes == 150);
        assert!(summary.total_allocations == 3);
        assert!(summary.peak_live_bytes == 90);
        assert!(summary.live_bytes_at_exit == 30);
        assert!(summary.raw_filename == format!("dhat.{}.json", summary.pid));
    }
    let folded = String::from_utf8(folded).unwrap();
    assert!(
        folded.contains(
            "Workload worker;crate::run (source.rs:1);crate::allocate (source.rs:12) 100\n"
        )
    );
    assert!(
        folded
            .contains("Nushell host;crate::run (source.rs:1);crate::allocate (source.rs:12) 50\n")
    );
    assert!(!folded.contains("malloc"));
}

#[test]
fn escapes_folded_stack_delimiters_without_losing_symbol_text() {
    let directory = tempfile::tempdir().unwrap();
    let mut data = profile(11, "kraai-perf profile-worker fixture");
    *data.pointer_mut("/ftbl/2").expect("missing test frame") =
        json!("0x456: crate::allocate<;>\nnext\rline\\path\t");
    write_profile(directory.path(), 11, &data);
    let mut folded = Vec::new();
    summarize(directory.path(), &mut folded).unwrap();
    let folded = String::from_utf8(folded).unwrap();
    assert!(folded.lines().count() == 2);
    assert!(folded.contains("crate::allocate<\\x3b>\\nnext\\rline\\\\path\\t"));
}

#[test]
fn rejects_invalid_profiles_and_counter_overflow() {
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
        let directory = tempfile::tempdir().unwrap();
        let mut data = profile(11, "kraai-perf profile-worker fixture");
        *data
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("missing test field {pointer}")) = replacement;
        write_profile(directory.path(), 11, &data);
        assert!(
            summarize(directory.path(), &mut Vec::new()).is_err(),
            "accepted invalid field {pointer}"
        );
    }
}

#[test]
fn global_peak_uses_live_snapshot_even_when_point_maximum_is_zero() {
    let directory = tempfile::tempdir().unwrap();
    let mut data = profile(11, "kraai-perf profile-worker fixture");
    *data.pointer_mut("/pps/0/mb").expect("missing test maximum") = json!(0);
    write_profile(directory.path(), 11, &data);
    let summaries = summarize(directory.path(), &mut Vec::new()).unwrap();
    let summary = summaries.first().expect("missing summary");
    assert!(summary.peak_live_bytes == 90);
    assert!(summary.live_bytes_at_exit == 30);
}

#[test]
fn rejects_missing_malformed_and_mismatched_process_profiles() {
    let directory = tempfile::tempdir().unwrap();
    assert!(summarize(directory.path(), &mut Vec::new()).is_err());
    fs::write(directory.path().join("dhat.11.json"), "unfinished JSON").unwrap();
    fs::write(directory.path().join("valgrind.11.log"), "DHAT fixture log").unwrap();
    assert!(summarize(directory.path(), &mut Vec::new()).is_err());
    let data = profile(11, "kraai-perf profile-worker fixture");
    write_profile(directory.path(), 11, &data);
    write_profile(directory.path(), 22, &data);
    let Err(error) = summarize(directory.path(), &mut Vec::new()) else {
        panic!("accepted a profile with the wrong process ID");
    };
    assert!(
        error
            .to_string()
            .contains("does not match filename process ID 22")
    );
}

#[test]
fn rejects_missing_child_profile_with_bounded_log_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    write_profile(
        directory.path(),
        11,
        &profile(11, "kraai-perf profile-worker nushell-execution"),
    );
    let message = "error: can't open DHAT output file for child";
    let log = format!("{message}\n{}diagnostic beyond limit", "x".repeat(16384));
    fs::write(directory.path().join("valgrind.22.log"), log).unwrap();
    let mut folded = Vec::new();
    let Err(error) = summarize(directory.path(), &mut folded) else {
        panic!("accepted a capture missing its child profile");
    };
    let error = error.to_string();
    assert!(error.contains("Missing DHAT profile for process 22"));
    assert!(error.contains(message));
    assert!(!error.contains("diagnostic beyond limit"));
    assert!(error.len() < 9000);
    assert!(folded.is_empty());
}

#[test]
fn rejects_profiles_without_logs_and_duplicate_artifact_ids() {
    let directory = tempfile::tempdir().unwrap();
    write_profile(
        directory.path(),
        11,
        &profile(11, "kraai-perf profile-worker fixture"),
    );
    fs::remove_file(directory.path().join("valgrind.11.log")).unwrap();
    let Err(error) = summarize(directory.path(), &mut Vec::new()) else {
        panic!("accepted a profile without its log");
    };
    assert!(
        error
            .to_string()
            .contains("Missing Valgrind log for process 11")
    );
    fs::write(directory.path().join("valgrind.11.log"), "DHAT fixture log").unwrap();
    fs::copy(
        directory.path().join("dhat.11.json"),
        directory.path().join("dhat.011.json"),
    )
    .unwrap();
    let Err(error) = summarize(directory.path(), &mut Vec::new()) else {
        panic!("accepted duplicate profile files for one process ID");
    };
    assert!(
        error
            .to_string()
            .contains("Duplicate Valgrind artifact for process ID 11")
    );
}

#[test]
fn command_traces_children_with_separate_artifacts_and_inline_frames() {
    let command = record_command(
        Path::new("/tmp/profile output"),
        Path::new("/tmp/kraai perf"),
        "nushell-execution",
    );
    assert!(command.get_program() == OsStr::new("valgrind"));
    let args: Vec<_> = command.get_args().collect();
    assert!(args.contains(&OsStr::new("--trace-children=yes")));
    assert!(args.contains(&OsStr::new("--read-inline-info=yes")));
    assert!(args.contains(&OsStr::new("--enable-debuginfod=no")));
    assert!(args.contains(&OsStr::new(
        "--dhat-out-file=/tmp/profile output/dhat.%p.json"
    )));
    assert!(args.ends_with(&[
        OsStr::new("/tmp/kraai perf"),
        OsStr::new("profile-worker"),
        OsStr::new("nushell-execution"),
    ]));
}
