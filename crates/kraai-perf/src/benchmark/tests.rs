use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, ensure};

use super::{Benchmark, Context, Workload, execute_phases, validate_catalog};

const PHASE_DELAY: Duration = Duration::from_millis(20);
const RUN_DELAY: Duration = Duration::from_millis(5);
const DROP_DELAY: Duration = Duration::from_millis(10);

struct Case<const FAIL_RUN: bool, const FAIL_VERIFY: bool, const SHARED_V2: bool = false>;

struct Fixture {
    value: u64,
    _cleanup: Cleanup,
}

struct Output {
    value: u64,
    _cleanup: Cleanup,
}

struct Cleanup(PathBuf);

struct EagerCase;

impl Benchmark for EagerCase {
    type Fixture = Fixture;
    type Output = Output;

    const NAME: &'static str = "eager-future-construction";
    const OPERATIONS: u64 = 1;
    const DESCRIPTION: &'static str = "Do work before returning the operation future";

    fn setup(context: &Context) -> impl Future<Output = Result<Self::Fixture>> + Send {
        Case::<false, false>::setup(context)
    }

    fn run(
        context: &Context,
        fixture: &mut Self::Fixture,
    ) -> impl Future<Output = Result<Self::Output>> + Send {
        std::thread::sleep(PHASE_DELAY);
        Case::<false, false>::run(context, fixture)
    }

    fn verify(
        context: &Context,
        fixture: Self::Fixture,
        output: Self::Output,
    ) -> impl Future<Output = Result<()>> + Send {
        Case::<false, false>::verify(context, fixture, output)
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        std::thread::sleep(DROP_DELAY);
        let _ = fs::write(&self.0, b"dropped");
    }
}

impl<const FAIL_RUN: bool, const FAIL_VERIFY: bool, const SHARED_V2: bool> Benchmark
    for Case<FAIL_RUN, FAIL_VERIFY, SHARED_V2>
{
    type Fixture = Fixture;
    type Output = Output;

    const NAME: &'static str = if FAIL_RUN { "failing-run" } else { "lifecycle" };
    const OPERATIONS: u64 = 2;
    const DESCRIPTION: &'static str = "Exercise benchmark lifecycle phases";
    const FIXTURES: &'static [&'static [u8]] = if SHARED_V2 {
        &[b"shared fixture version 2"]
    } else {
        &[b"shared fixture version 1"]
    };

    async fn setup(context: &Context) -> Result<Self::Fixture> {
        ensure!(!context.directory.join("phase").exists(), "setup ran twice");
        fs::write(context.directory.join("phase"), b"setup")?;
        fs::write(
            context.directory.join("profiling"),
            context.profiling.to_string(),
        )?;
        tokio::time::sleep(PHASE_DELAY).await;
        Ok(Fixture {
            value: 41,
            _cleanup: Cleanup(context.directory.join("fixture-dropped")),
        })
    }

    async fn run(context: &Context, fixture: &mut Self::Fixture) -> Result<Self::Output> {
        let phase = fs::read_to_string(context.directory.join("phase"))?;
        ensure!(phase == "setup", "run did not follow setup");
        fs::write(context.directory.join("phase"), b"run")?;
        fixture.value += 1;
        tokio::time::sleep(RUN_DELAY).await;
        ensure!(!FAIL_RUN, "intentional run failure");
        Ok(Output {
            value: fixture.value * 2,
            _cleanup: Cleanup(context.directory.join("output-dropped")),
        })
    }

    async fn verify(context: &Context, fixture: Self::Fixture, output: Self::Output) -> Result<()> {
        let phase = fs::read_to_string(context.directory.join("phase"))?;
        ensure!(phase == "run", "verification did not follow run");
        ensure!(
            fixture.value == 42 && output.value == 84,
            "verification received the wrong state"
        );
        ensure!(
            !context.directory.join("fixture-dropped").exists(),
            "fixture dropped before verification"
        );
        ensure!(
            !context.directory.join("output-dropped").exists(),
            "output dropped before verification"
        );
        fs::write(context.directory.join("phase"), b"verify")?;
        tokio::time::sleep(PHASE_DELAY).await;
        ensure!(!FAIL_VERIFY, "intentional verification failure");
        drop(output);
        drop(fixture);
        Ok(())
    }
}

#[test]
fn measures_only_run_and_verifies_before_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let context = Context {
        directory: directory.path().to_path_buf(),
        profiling: true,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let started = Instant::now();
    let sample = runtime
        .block_on(execute_phases::<Case<false, false>>(&context))
        .unwrap();
    let elapsed = started.elapsed();
    drop(runtime);
    let excluded = 2 * PHASE_DELAY.as_nanos() + 2 * DROP_DELAY.as_nanos();
    assert!(
        elapsed.as_nanos() >= u128::from(sample.wall_ns) + excluded,
        "sample included setup, verification, or cleanup delay"
    );
    assert!(
        u128::from(sample.wall_ns) >= RUN_DELAY.as_nanos(),
        "run delay was not measured"
    );
    let phase = fs::read_to_string(directory.path().join("phase")).unwrap();
    assert!(phase == "verify", "verification did not complete");
    let profiling = fs::read_to_string(directory.path().join("profiling")).unwrap();
    assert!(
        profiling == "true",
        "profiling context was not passed to setup"
    );
    for marker in ["fixture-dropped", "output-dropped"] {
        let state = fs::read_to_string(directory.path().join(marker)).unwrap();
        assert!(state == "dropped", "benchmark state was not cleaned up");
    }
}

#[test]
fn failing_run_skips_verification_and_returns_no_sample() {
    let directory = tempfile::tempdir().unwrap();
    let context = Context {
        directory: directory.path().to_path_buf(),
        profiling: false,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let result = runtime.block_on(execute_phases::<Case<true, false>>(&context));
    drop(runtime);
    let failed = result.is_err_and(|error| error.to_string() == "intentional run failure");
    assert!(failed, "failed run returned a sample or changed its error");
    let phase = fs::read_to_string(directory.path().join("phase")).unwrap();
    assert!(phase == "run", "verification ran after a failed operation");
    assert!(
        directory.path().join("fixture-dropped").exists(),
        "failed run leaked fixture"
    );
    assert!(
        !directory.path().join("output-dropped").exists(),
        "failed run unexpectedly produced output"
    );
}

#[test]
fn failing_verification_rejects_the_sample_and_cleans_up() {
    let directory = tempfile::tempdir().unwrap();
    let context = Context {
        directory: directory.path().to_path_buf(),
        profiling: false,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let result = runtime.block_on(execute_phases::<Case<false, true>>(&context));
    drop(runtime);
    let failed = result.is_err_and(|error| error.to_string() == "intentional verification failure");
    assert!(
        failed,
        "failed verification returned a sample or changed its error"
    );
    for marker in ["fixture-dropped", "output-dropped"] {
        assert!(
            directory.path().join(marker).exists(),
            "failed verification leaked benchmark state"
        );
    }
}

#[test]
fn workload_dispatches_its_registered_executor() {
    let workload = Workload::new::<Case<true, false>>(b"failing case source");
    let result = workload.sample(false);
    let failed = result.is_err_and(|error| error.to_string() == "intentional run failure");
    assert!(failed, "workload did not dispatch its registered benchmark");
}

#[test]
fn operation_future_construction_is_inside_measurement() {
    let workload = Workload::new::<EagerCase>(b"eager case source");
    let sample = workload.sample(false).unwrap();
    assert!(
        u128::from(sample.wall_ns) >= PHASE_DELAY.as_nanos(),
        "work done while constructing the operation future was excluded"
    );
}

#[test]
fn fingerprints_follow_case_source_and_shared_fixtures() {
    let original = Workload::new::<Case<false, false>>(b"case source version 1");
    let source_changed = Workload::new::<Case<false, false>>(b"case source version 2");
    let fixture_changed = Workload::new::<Case<false, false, true>>(b"case source version 1");
    assert!(
        original.fingerprint != source_changed.fingerprint,
        "case source change did not change its identity"
    );
    assert!(
        original.fingerprint != fixture_changed.fingerprint,
        "shared fixture change did not change case identity"
    );

    let before = validate_catalog(vec![
        original.clone(),
        Workload::new::<Case<true, false>>(b"another case version 1"),
    ])
    .unwrap();
    let after = validate_catalog(vec![
        original.clone(),
        Workload::new::<Case<true, false>>(b"another case version 2"),
    ])
    .unwrap();
    let find_identity = |catalog: &[Workload]| {
        catalog
            .iter()
            .find(|workload| workload.name == original.name)
            .map(|workload| workload.fingerprint.clone())
    };
    assert!(
        find_identity(&before) == find_identity(&after),
        "unrelated case changed existing identity"
    );
}

#[test]
fn catalog_rejects_invalid_registration_and_finds_unique_builtins() {
    let valid = Workload::new::<Case<false, false>>(b"valid case");
    assert!(
        validate_catalog(vec![valid.clone(), valid.clone()]).is_err(),
        "duplicate names were accepted"
    );
    let mut unnamed = valid.clone();
    unnamed.name = "  ".into();
    assert!(
        validate_catalog(vec![unnamed]).is_err(),
        "empty name was accepted"
    );
    let mut empty = valid;
    empty.operations = 0;
    assert!(
        validate_catalog(vec![empty]).is_err(),
        "zero operations were accepted"
    );

    let catalog = crate::workloads::catalog().unwrap();
    assert!(!catalog.is_empty(), "no built-in benchmarks are registered");
    let mut names = BTreeSet::new();
    for workload in catalog {
        assert!(
            names.insert(workload.name.clone()),
            "built-in names are not unique"
        );
        let found = crate::workloads::find(&workload.name).unwrap();
        assert!(
            found.fingerprint == workload.fingerprint,
            "lookup selected a different benchmark"
        );
    }
    assert!(
        crate::workloads::find("unregistered-test-case").is_err(),
        "unknown workload was accepted"
    );
}
