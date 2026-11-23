use std::hint::black_box;
use std::mem::DropGuard;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::resource::{UsageWho, getrusage};
use nix::sys::time::TimeValLike;

use crate::test_support::run_fixture;

use super::supported::{cpu_delta_ns, measure, measure_async};

const ALLOCATION_BYTES: usize = 64 * 1024 * 1024;

#[test]
fn measures_worker_and_child_resources() {
    run_fixture(module_path!(), "resource_measurement_fixture");
}

#[test]
#[ignore = "invoked in a separate process to isolate resource accounting"]
fn resource_measurement_fixture() {
    let idle = measure(|| Ok(())).unwrap();
    let busy = measure(|| {
        consume_resources();
        Ok(())
    })
    .unwrap();
    assert!(busy.wall_ns > 0, "busy workload recorded no elapsed time");
    assert!(
        busy.user_cpu_ns + busy.system_cpu_ns > 0,
        "busy workload recorded no CPU time"
    );
    assert!(
        busy.peak_rss_bytes >= ALLOCATION_BYTES as u64,
        "touched memory was not recorded"
    );
    assert!(
        busy.peak_rss_bytes > idle.peak_rss_bytes,
        "allocating and touching memory did not increase measured peak RSS"
    );
    assert!(
        busy.children_user_cpu_ns == 0,
        "unexpected child user CPU time"
    );
    assert!(
        busy.children_system_cpu_ns == 0,
        "unexpected child system CPU time"
    );
    assert!(
        busy.children_peak_rss_bytes == 0,
        "unexpected child resident memory"
    );

    let child = measure(|| {
        run_fixture(module_path!(), "child_workload_fixture");
        Ok(())
    })
    .unwrap();
    assert!(child.wall_ns > 0, "child workload recorded no elapsed time");
    assert!(
        child.children_user_cpu_ns + child.children_system_cpu_ns > 0,
        "child CPU time was not recorded"
    );
    assert!(
        child.children_peak_rss_bytes >= ALLOCATION_BYTES as u64,
        "child touched memory was not recorded"
    );

    let sleeping = measure(|| {
        std::thread::sleep(Duration::from_millis(250));
        Ok(())
    })
    .unwrap();
    assert!(
        sleeping.wall_ns >= 250_000_000,
        "sleep duration was not recorded"
    );
    assert!(
        sleeping.user_cpu_ns + sleeping.system_cpu_ns < sleeping.wall_ns / 2,
        "sleep was counted as CPU time"
    );
    let busy_cpu_ns = busy.user_cpu_ns + busy.system_cpu_ns;
    let sleeping_cpu_ns = sleeping.user_cpu_ns + sleeping.system_cpu_ns;
    assert!(
        busy_cpu_ns > sleeping_cpu_ns,
        "busy workload did not use more measured CPU time than sleeping"
    );
    assert!(
        sleeping.peak_rss_bytes >= busy.peak_rss_bytes,
        "worker lifetime peak decreased"
    );
    assert!(
        sleeping.children_user_cpu_ns == 0,
        "previous child user CPU time was counted again"
    );
    assert!(
        sleeping.children_system_cpu_ns == 0,
        "previous child system CPU time was counted again"
    );
    assert!(
        sleeping.children_peak_rss_bytes >= child.children_peak_rss_bytes,
        "child lifetime peak decreased"
    );
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
fn cpu_deltas_reject_decreasing_and_overflowing_counters() {
    let delta = cpu_delta_ns(990_000, 1_010_000).unwrap();
    assert!(delta == 20_000_000, "CPU delta has incorrect units");
    let unchanged = cpu_delta_ns(50, 50).unwrap();
    assert!(unchanged == 0, "unchanged CPU counter produced a delta");
    assert!(
        cpu_delta_ns(51, 50).is_err(),
        "decreasing CPU counters were accepted"
    );
    assert!(
        cpu_delta_ns(0, i64::MAX).is_err(),
        "CPU nanosecond overflow was accepted"
    );
    assert!(
        cpu_delta_ns(i64::MIN, i64::MAX).is_err(),
        "CPU subtraction overflow was accepted"
    );
}

#[test]
fn async_measurement_excludes_surrounding_phases() {
    run_fixture(module_path!(), "async_scope_fixture");
}

#[test]
#[ignore = "invoked in a separate process to isolate async resource accounting"]
fn async_scope_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let operation = measure_async(async {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(DropGuard::new(dropped.clone(), |dropped| {
            dropped.store(true, Ordering::Relaxed);
        }))
    });

    let setup = measure(|| {
        consume_resources();
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    })
    .unwrap();
    let outer_before = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    let outer_started = Instant::now();
    let (output, sample) = runtime.block_on(operation).unwrap();
    let outer_wall_ns = outer_started.elapsed().as_nanos();
    let outer_after = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    drop(runtime);
    let outer_cpu_ns = cpu_delta_ns(
        outer_before.user_time().num_microseconds(),
        outer_after.user_time().num_microseconds(),
    )
    .unwrap()
        + cpu_delta_ns(
            outer_before.system_time().num_microseconds(),
            outer_after.system_time().num_microseconds(),
        )
        .unwrap();

    consume_resources();
    std::thread::sleep(Duration::from_millis(50));
    assert!(sample.wall_ns >= 10_000_000, "async wait was not measured");
    assert!(
        u128::from(sample.wall_ns) <= outer_wall_ns,
        "measurement included time outside polling the operation"
    );
    assert!(
        sample.user_cpu_ns + sample.system_cpu_ns <= outer_cpu_ns,
        "measurement included CPU work outside polling the operation"
    );
    assert!(
        sample.peak_rss_bytes >= setup.peak_rss_bytes,
        "measurement lost the lifetime memory peak from setup"
    );
    assert!(
        !dropped.load(Ordering::Relaxed),
        "operation output was dropped before verification"
    );
    drop(output);
    assert!(
        dropped.load(Ordering::Relaxed),
        "operation output was not returned to the caller"
    );
}
