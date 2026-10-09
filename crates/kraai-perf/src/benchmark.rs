use std::future::Future;
use std::path::PathBuf;

use color_eyre::eyre::{Result, ensure};
use serde::Serialize;

use crate::report::Sample;

pub(crate) struct Context {
    pub directory: PathBuf,
    pub profiling: bool,
}

pub(crate) trait Benchmark {
    type Fixture: Send;
    type Output: Send;

    const NAME: &'static str;
    const OPERATIONS: u64;
    const DESCRIPTION: &'static str;
    const FIXTURES: &'static [&'static [u8]] = &[];

    fn setup(context: &Context) -> impl Future<Output = Result<Self::Fixture>> + Send;
    fn run(
        context: &Context,
        fixture: &mut Self::Fixture,
    ) -> impl Future<Output = Result<Self::Output>> + Send;
    fn verify(
        context: &Context,
        fixture: Self::Fixture,
        output: Self::Output,
    ) -> impl Future<Output = Result<()>> + Send;
}

#[derive(Clone, Serialize)]
pub(crate) struct Workload {
    pub name: String,
    pub operations: u64,
    pub description: String,
    pub fingerprint: String,
    #[serde(skip)]
    execute: fn(bool) -> Result<Sample>,
}

impl Workload {
    pub fn new<B: Benchmark>(source: &[u8]) -> Self {
        let mut sources = vec![source];
        sources.extend_from_slice(B::FIXTURES);
        Self {
            name: B::NAME.into(),
            operations: B::OPERATIONS,
            description: B::DESCRIPTION.into(),
            fingerprint: crate::fingerprint::sources(&sources),
            execute: execute::<B>,
        }
    }

    pub fn sample(&self, profiling: bool) -> Result<Sample> {
        (self.execute)(profiling)
    }
}

fn execute<B: Benchmark>(profiling: bool) -> Result<Sample> {
    let directory = tempfile::tempdir_in(std::env::current_dir()?)?;
    let context = Context {
        directory: directory.path().to_path_buf(),
        profiling,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        #[cfg(unix)]
        {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                biased;
                _ = terminate.recv() => color_eyre::eyre::bail!("Performance workload cancelled"),
                result = execute_phases::<B>(&context) => result,
            }
        }
        #[cfg(not(unix))]
        execute_phases::<B>(&context).await
    });
    drop(runtime);
    directory.close()?;
    result
}

async fn execute_phases<B: Benchmark>(context: &Context) -> Result<Sample> {
    let mut fixture = B::setup(context).await?;
    let (output, sample) = crate::measurement::measure_async(async {
        let output = B::run(context, std::hint::black_box(&mut fixture)).await?;
        Ok(std::hint::black_box(output))
    })
    .await?;
    B::verify(context, fixture, output).await?;
    Ok(sample)
}

macro_rules! register_workloads {
    ($($module:ident),+ $(,)?) => {
        $(mod $module;)+

        pub(crate) fn catalog() -> color_eyre::eyre::Result<Vec<crate::benchmark::Workload>> {
            crate::benchmark::validate_catalog(vec![$(
                crate::benchmark::Workload::new::<$module::Case>(include_bytes!(concat!(
                    "workloads/", stringify!($module), ".rs"
                )))
            ),+])
        }
    };
}

pub(crate) use register_workloads;

pub(crate) fn validate_catalog(workloads: Vec<Workload>) -> Result<Vec<Workload>> {
    let mut names = std::collections::BTreeSet::new();
    for workload in &workloads {
        ensure!(!workload.name.trim().is_empty(), "Empty workload name");
        ensure!(
            workload.operations > 0,
            "Workload {} has no operations",
            workload.name
        );
        ensure!(
            names.insert(&workload.name),
            "Duplicate workload {}",
            workload.name
        );
    }
    Ok(workloads)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[path = "benchmark_tests.rs"]
mod tests;
