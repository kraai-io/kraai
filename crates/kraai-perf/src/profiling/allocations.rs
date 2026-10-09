use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{Context, Result, ensure, eyre};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub(super) struct AllocationSummary {
    pub(super) command: String,
    pub(super) pid: u32,
    pub(super) raw_filename: String,
    pub(super) total_allocated_bytes: u64,
    pub(super) total_allocations: u64,
    pub(super) peak_live_bytes: u64,
    pub(super) live_bytes_at_exit: u64,
}

#[derive(Deserialize)]
struct Profile {
    #[serde(rename = "dhatFileVersion")]
    version: u32,
    mode: String,
    bklt: bool,
    cmd: String,
    pid: u32,
    pps: Vec<ProgramPoint>,
    ftbl: Vec<String>,
}

#[derive(Deserialize)]
struct ProgramPoint {
    tb: u64,
    tbk: u64,
    gb: u64,
    eb: u64,
    fs: Vec<usize>,
}

pub(super) fn record_command(directory: &Path, executable: &Path, workload: &str) -> Command {
    let mut command = Command::new("valgrind");
    command
        .args([
            "--tool=dhat",
            "--trace-children=yes",
            "--num-callers=64",
            "--read-inline-info=yes",
            "--enable-debuginfod=no",
        ])
        .arg(path_option(
            "--dhat-out-file=",
            &directory.join("dhat.%p.json"),
        ))
        .arg(path_option(
            "--log-file=",
            &directory.join("valgrind.%p.log"),
        ))
        .arg(executable)
        .args(["profile-worker", workload]);
    command
}

pub(super) fn summarize(
    directory: &Path,
    folded: &mut impl Write,
) -> Result<Vec<AllocationSummary>> {
    let mut summaries = Vec::new();
    for (pid, path) in profile_paths(directory)? {
        let profile: Profile = serde_json::from_reader(BufReader::new(File::open(&path)?))
            .wrap_err_with(|| format!("read DHAT profile {}", path.display()))?;
        ensure!(
            profile.version == 2,
            "Unsupported DHAT version {}",
            profile.version
        );
        ensure!(
            profile.mode == "heap" && profile.bklt,
            "DHAT profile is not a heap lifetime profile"
        );
        ensure!(
            profile.pid == pid,
            "DHAT profile process ID {} does not match filename process ID {pid}",
            profile.pid
        );
        ensure!(
            !profile.cmd.trim().is_empty(),
            "DHAT profile has no command"
        );
        ensure!(
            profile.ftbl.first().is_some_and(|frame| frame == "[root]"),
            "DHAT frame table has no root"
        );
        let raw_filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| eyre!("DHAT profile filename is not UTF-8"))?
            .to_owned();
        let mut summary = AllocationSummary {
            command: profile.cmd.clone(),
            pid: profile.pid,
            raw_filename,
            total_allocated_bytes: 0,
            total_allocations: 0,
            peak_live_bytes: 0,
            live_bytes_at_exit: 0,
        };
        let root = process_label(&profile);
        for point in profile.pps {
            ensure!(
                point.gb <= point.tb && point.eb <= point.tb,
                "DHAT live bytes exceed total allocated bytes"
            );
            ensure!(
                point.tb == 0 || point.tbk > 0,
                "DHAT reports bytes without allocations"
            );
            ensure!(!point.fs.is_empty(), "DHAT allocation has no stack frames");
            let mut stack = root.clone();
            for index in point.fs.iter().rev() {
                let frame = profile
                    .ftbl
                    .get(*index)
                    .ok_or_else(|| eyre!("DHAT frame index {index} is outside its frame table"))?;
                if frame != "[root]" && !is_profiler_allocator(frame) {
                    write!(stack, ";{}", sanitize_frame(strip_address(frame)))?;
                }
            }
            checked_add(&mut summary.total_allocated_bytes, point.tb)?;
            checked_add(&mut summary.total_allocations, point.tbk)?;
            checked_add(&mut summary.peak_live_bytes, point.gb)?;
            checked_add(&mut summary.live_bytes_at_exit, point.eb)?;
            if point.tb > 0 {
                writeln!(folded, "{stack} {}", point.tb)?;
            }
        }
        summaries.push(summary);
    }
    Ok(summaries)
}

fn profile_paths(directory: &Path) -> Result<BTreeMap<u32, PathBuf>> {
    let mut profiles = BTreeMap::new();
    let mut logs = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let (pid, files) = if let Some(pid) = name
            .strip_prefix("dhat.")
            .and_then(|name| name.strip_suffix(".json"))
        {
            (pid, &mut profiles)
        } else if let Some(pid) = name
            .strip_prefix("valgrind.")
            .and_then(|name| name.strip_suffix(".log"))
        {
            (pid, &mut logs)
        } else {
            continue;
        };
        let pid = pid
            .parse::<u32>()
            .wrap_err_with(|| format!("invalid Valgrind artifact process ID in {name}"))?;
        ensure!(pid > 0, "invalid Valgrind artifact process ID in {name}");
        ensure!(
            entry.metadata()?.is_file(),
            "Valgrind artifact is not a file: {name}"
        );
        ensure!(
            files.insert(pid, entry.path()).is_none(),
            "Duplicate Valgrind artifact for process ID {pid}: {name}"
        );
    }
    for (pid, log) in &logs {
        ensure!(
            profiles.contains_key(pid),
            "Missing DHAT profile for process {pid}; Valgrind log: {}",
            crate::process::read_error(log)
                .unwrap_or_else(|error| format!("could not read log: {error}"))
        );
    }
    ensure!(
        !profiles.is_empty(),
        "Valgrind did not produce any DHAT profiles"
    );
    for pid in profiles.keys() {
        ensure!(
            logs.contains_key(pid),
            "Missing Valgrind log for process {pid}"
        );
    }
    Ok(profiles)
}

fn path_option(option: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(option);
    argument.push(path);
    argument
}

fn process_label(profile: &Profile) -> String {
    if profile
        .cmd
        .split_whitespace()
        .any(|argument| argument == "--kraai-internal-nushell-host")
    {
        String::from("Nushell host")
    } else if profile
        .cmd
        .split_whitespace()
        .any(|argument| argument == "profile-worker")
    {
        String::from("Workload worker")
    } else {
        format!("Process {}", profile.pid)
    }
}

fn checked_add(total: &mut u64, value: u64) -> Result<()> {
    *total = total
        .checked_add(value)
        .ok_or_else(|| eyre!("DHAT allocation totals exceed u64"))?;
    Ok(())
}

fn strip_address(frame: &str) -> &str {
    match frame.split_once(": ") {
        Some((address, symbol))
            if address.strip_prefix("0x").is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_hexdigit())
            }) =>
        {
            symbol
        }
        _ => frame,
    }
}

fn is_profiler_allocator(frame: &str) -> bool {
    let symbol = strip_address(frame).split_whitespace().next();
    matches!(
        symbol,
        Some(
            "malloc"
                | "calloc"
                | "realloc"
                | "memalign"
                | "aligned_alloc"
                | "posix_memalign"
                | "valloc"
                | "pvalloc"
        )
    ) && (frame.contains("vgpreload_dhat-") || frame.contains("vg_replace_malloc.c:"))
}

fn sanitize_frame(frame: &str) -> String {
    frame
        .replace('\\', "\\\\")
        .replace(';', "\\x3b")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
#[path = "allocations_tests.rs"]
mod tests;
