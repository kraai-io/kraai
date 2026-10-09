use std::path::Path;
use std::process::Command;

pub(super) const EVENT: &str = "cpu-clock:u";
pub(super) const FREQUENCY_HZ: u16 = 997;
pub(super) const CALL_GRAPH: &str = "dwarf,16384";

pub(super) fn record_command(raw: &Path, executable: &Path, workload: &str) -> Command {
    let mut command = Command::new("perf");
    command
        .args(["record", "--event", EVENT, "--freq"])
        .arg(FREQUENCY_HZ.to_string())
        .args(["--call-graph", CALL_GRAPH, "--output"])
        .arg(raw)
        .arg("--")
        .arg(executable)
        .args(["profile-worker", workload]);
    command
}

pub(super) fn decode_command(raw: &Path) -> Command {
    let mut command = Command::new("perf");
    command.args(["script", "--input"]).arg(raw).args([
        "--demangle",
        "--inline",
        "--fields=-period",
    ]);
    command
}

pub(super) fn collapse_command(input: &Path) -> Command {
    let mut command = Command::new("inferno-collapse-perf");
    command.args(["--event-filter", "cpu-clock"]).arg(input);
    command
}
