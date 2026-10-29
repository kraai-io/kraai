use color_eyre::eyre::{Result, ensure, eyre};
use kraai_runtime::{CreateSessionRequest, RuntimeHandle, RuntimeStartupState};
use kraai_types::SessionId;

use crate::benchmark::{Benchmark, Context};
use crate::fixtures::{provider, runtime};

pub(super) struct Case;

impl Benchmark for Case {
    type Fixture = ();
    type Output = (RuntimeHandle, RuntimeStartupState, SessionId);

    const NAME: &str = "runtime-first-session";
    const OPERATIONS: u64 = 1;
    const DESCRIPTION: &str = "Relaunch the runtime on an existing database with an offline provider, wait until it is ready and create the session for a first message";
    const FIXTURES: &[&[u8]] = runtime::SOURCES;

    async fn setup(context: &Context) -> Result<Self::Fixture> {
        runtime::prepare(&context.directory)?;
        let (handle, startup, _session) = first_session(context).await?;
        handle.shutdown().await.map_err(|error| eyre!(error))?;
        ensure!(
            startup == RuntimeStartupState::ready(),
            "Initial launch did not become ready: {startup:?}"
        );
        Ok(())
    }

    async fn run(context: &Context, _fixture: &mut Self::Fixture) -> Result<Self::Output> {
        first_session(context).await
    }

    async fn verify(
        _context: &Context,
        _fixture: Self::Fixture,
        output: Self::Output,
    ) -> Result<()> {
        let (handle, startup, session) = output;
        let checks = async {
            ensure!(
                startup == RuntimeStartupState::ready(),
                "Runtime did not become ready: {startup:?}"
            );
            let models = handle.list_models().await?;
            ensure!(
                models
                    .get(provider::provider_id().as_str())
                    .is_some_and(|models| models
                        .iter()
                        .any(|model| model.id == provider::model_id().as_str())),
                "Offline model was not listed"
            );
            let sessions = handle.list_sessions().await?;
            ensure!(
                sessions.len() == 2 && sessions.iter().any(|known| known.id == session),
                "Relaunch did not keep both sessions"
            );
            Ok::<(), color_eyre::Report>(())
        }
        .await;
        handle.shutdown().await.map_err(|error| eyre!(error))?;
        checks
    }
}

async fn first_session(
    context: &Context,
) -> Result<(RuntimeHandle, RuntimeStartupState, SessionId)> {
    let handle = runtime::start(&context.directory);
    let startup = handle.wait_for_startup().await?;
    let session = handle
        .create_session_with(CreateSessionRequest {
            workspace_dir: Some(runtime::workspace(&context.directory).display().to_string()),
            profile_id: None,
        })
        .await?;
    Ok((handle, startup, session))
}
