use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, ensure, eyre};
use nix::sys::resource::{UsageWho, getrusage};
use nix::sys::time::TimeValLike;

use crate::test_support::run_fixture;

use super::supported::{cpu_delta_ns, measure, measure_async};

const ALLOCATION_BYTES: usize = 64 * 1024 * 1024;

#[test]
fn measures_worker_and_child_resources() -> Result<()> {
    run_fixture(module_path!(), "resource_measurement_fixture")
}

#[test]
#[ignore = "invoked in a separate process to isolate resource accounting"]
fn resource_measurement_fixture() -> Result<()> {
    let idle = measure(|| Ok(()))?;
    let busy = measure(|| {
        consume_resources();
        Ok(())
    })?;
    ensure!(busy.wall_ns > 0, "busy workload recorded no elapsed time");
    ensure!(
        busy.user_cpu_ns + busy.system_cpu_ns > 0,
        "busy workload recorded no CPU time"
    );
    ensure!(
        busy.peak_rss_bytes >= ALLOCATION_BYTES as u64,
        "touched memory was not recorded"
    );
    ensure!(
        busy.peak_rss_bytes > idle.peak_rss_bytes,
        "allocating and touching memory did not increase measured peak RSS"
    );
    ensure!(
        busy.children_user_cpu_ns == 0,
        "unexpected child user CPU time"
    );
    ensure!(
        busy.children_system_cpu_ns == 0,
        "unexpected child system CPU time"
    );
    ensure!(
        busy.children_peak_rss_bytes == 0,
        "unexpected child resident memory"
    );

    let child = measure(|| run_fixture(module_path!(), "child_workload_fixture"))?;
    ensure!(child.wall_ns > 0, "child workload recorded no elapsed time");
    ensure!(
        child.children_user_cpu_ns + child.children_system_cpu_ns > 0,
        "child CPU time was not recorded"
    );
    ensure!(
        child.children_peak_rss_bytes >= ALLOCATION_BYTES as u64,
        "child touched memory was not recorded"
    );

    let sleeping = measure(|| {
        std::thread::sleep(Duration::from_millis(250));
        Ok(())
    })?;
    ensure!(
        sleeping.wall_ns >= 250_000_000,
        "sleep duration was not recorded"
    );
    ensure!(
        sleeping.user_cpu_ns + sleeping.system_cpu_ns < sleeping.wall_ns / 2,
        "sleep was counted as CPU time"
    );
    let busy_cpu_ns = busy.user_cpu_ns + busy.system_cpu_ns;
    let sleeping_cpu_ns = sleeping.user_cpu_ns + sleeping.system_cpu_ns;
    ensure!(
        busy_cpu_ns > sleeping_cpu_ns,
        "busy workload did not use more measured CPU time than sleeping"
    );
    ensure!(
        sleeping.peak_rss_bytes >= busy.peak_rss_bytes,
        "worker lifetime peak decreased"
    );
    ensure!(
        sleeping.children_user_cpu_ns == 0,
        "previous child user CPU time was counted again"
    );
    ensure!(
        sleeping.children_system_cpu_ns == 0,
        "previous child system CPU time was counted again"
    );
    ensure!(
        sleeping.children_peak_rss_bytes >= child.children_peak_rss_bytes,
        "child lifetime peak decreased"
    );
    Ok(())
}

#[test]
#[ignore = "invoked by the child resource measurement test"]
fn child_workload_fixture() {
    consume_resources();
}

fn consume_resources() {
    let mut memory = vec![0_u8; ALLOCATION_BYTES];
    for byte in memory.iter_mut().step_by(4096) {
        *byte = 1;
    }
    black_box(&memory);
    let mut value = 1_u64;
    for _ in 0..2_000_000 {
        value = black_box(value.wrapping_mul(6364136223846793005).rotate_left(7));
    }
    black_box(value);
}

#[test]
fn cpu_deltas_reject_decreasing_and_overflowing_counters() -> Result<()> {
    let delta = cpu_delta_ns(990_000, 1_010_000)?;
    ensure!(delta == 20_000_000, "CPU delta has incorrect units");
    let unchanged = cpu_delta_ns(50, 50)?;
    ensure!(unchanged == 0, "unchanged CPU counter produced a delta");
    ensure!(
        cpu_delta_ns(51, 50).is_err(),
        "decreasing CPU counters were accepted"
    );
    ensure!(
        cpu_delta_ns(0, i64::MAX).is_err(),
        "CPU nanosecond overflow was accepted"
    );
    ensure!(
        cpu_delta_ns(i64::MIN, i64::MAX).is_err(),
        "CPU subtraction overflow was accepted"
    );
    Ok(())
}

#[test]
fn failed_workloads_do_not_produce_samples() {
    let result = measure(|| Err(eyre!("workload failed")));
    assert!(result.is_err_and(|error| error.to_string() == "workload failed"));
}

#[test]
fn async_measurement_excludes_surrounding_phases() -> Result<()> {
    run_fixture(module_path!(), "async_scope_fixture")
}

#[test]
#[ignore = "invoked in a separate process to isolate async resource accounting"]
fn async_scope_fixture() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let dropped = Arc::new(AtomicBool::new(false));
    let operation = measure_async(async {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(OutputGuard(dropped.clone()))
    });

    let setup = measure(|| {
        consume_resources();
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    })?;
    let outer_before = getrusage(UsageWho::RUSAGE_SELF)?;
    let outer_started = Instant::now();
    let (output, sample) = runtime.block_on(operation)?;
    let outer_wall_ns = outer_started.elapsed().as_nanos();
    let outer_after = getrusage(UsageWho::RUSAGE_SELF)?;
    drop(runtime);
    let outer_cpu_ns = cpu_delta_ns(
        outer_before.user_time().num_microseconds(),
        outer_after.user_time().num_microseconds(),
    )? + cpu_delta_ns(
        outer_before.system_time().num_microseconds(),
        outer_after.system_time().num_microseconds(),
    )?;

    consume_resources();
    std::thread::sleep(Duration::from_millis(50));
    ensure!(sample.wall_ns >= 10_000_000, "async wait was not measured");
    ensure!(
        u128::from(sample.wall_ns) <= outer_wall_ns,
        "measurement included time outside polling the operation"
    );
    ensure!(
        sample.user_cpu_ns + sample.system_cpu_ns <= outer_cpu_ns,
        "measurement included CPU work outside polling the operation"
    );
    ensure!(
        sample.peak_rss_bytes >= setup.peak_rss_bytes,
        "measurement lost the lifetime memory peak from setup"
    );
    ensure!(
        !dropped.load(Ordering::Relaxed),
        "operation output was dropped before verification"
    );
    drop(output);
    ensure!(
        dropped.load(Ordering::Relaxed),
        "operation output was not returned to the caller"
    );
    Ok(())
}

struct OutputGuard(Arc<AtomicBool>);

impl Drop for OutputGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[test]
fn failed_async_workloads_do_not_produce_samples() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let result = runtime.block_on(measure_async(async {
        tokio::task::yield_now().await;
        Err::<(), _>(eyre!("async workload failed"))
    }));
    drop(runtime);
    let failed = result.is_err_and(|error| error.to_string() == "async workload failed");
    ensure!(
        failed,
        "failed async workload produced a sample or changed its error"
    );
    Ok(())
}
