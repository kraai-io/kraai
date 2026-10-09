use std::time::Duration;

use color_eyre::eyre::{Result, ensure};
use kraai_nushell_runtime::{
    INTERNAL_HOST_ARGUMENT, ScriptExecutionPlan, ScriptExecutionResult, execute,
};
use kraai_sandbox::Termination;
use kraai_types::{SandboxCapabilities, SandboxCapability, ScriptExecutionId};
use tokio_util::sync::CancellationToken;

use crate::benchmark::{Benchmark, Context};

pub(super) struct Case;

impl Benchmark for Case {
    type Fixture = Option<ScriptExecutionPlan>;
    type Output = ScriptExecutionResult;

    const NAME: &'static str = "nushell-execution";
    const OPERATIONS: u64 = 1;
    const DESCRIPTION: &'static str = "Start a Nushell host without the OS sandbox, map 4096 integers and return their sum through the private transport";

    async fn setup(context: &Context) -> Result<Self::Fixture> {
        let mut plan = ScriptExecutionPlan::new(
            ScriptExecutionId::new("performance-script"),
            std::env::current_exe()?,
            b"1..4096 | each {|number| $number * 2 } | math sum | to json --raw".to_vec(),
            context.directory.clone(),
            SandboxCapabilities::new([SandboxCapability::NoSandbox])?,
            Duration::from_secs(10),
        );
        if context.profiling {
            plan.startup_timeout = Duration::from_secs(60);
            plan.timeout = Duration::from_secs(60);
        }
        plan.host_arguments.push(INTERNAL_HOST_ARGUMENT.into());
        plan.environment.insert("TERM".into(), "dumb".into());
        Ok(Some(plan))
    }

    async fn run(_context: &Context, fixture: &mut Self::Fixture) -> Result<Self::Output> {
        let plan = fixture
            .take()
            .ok_or_else(|| color_eyre::eyre::eyre!("Nushell fixture was already consumed"))?;
        Ok(execute(plan, CancellationToken::new()).await?)
    }

    async fn verify(
        _context: &Context,
        _fixture: Self::Fixture,
        result: Self::Output,
    ) -> Result<()> {
        ensure!(
            result.output.termination == Termination::Exited { code: Some(0) },
            "Nushell workload failed: {:?}, {}",
            result.output.termination,
            String::from_utf8_lossy(&result.output.stderr)
        );
        ensure!(
            result.output.stdout == b"16781312\n",
            "Nushell produced an incorrect sum"
        );
        ensure!(
            result.output.stderr.is_empty(),
            "Nushell wrote unexpected stderr"
        );
        Ok(())
    }
}
