use color_eyre::eyre::{Result, ensure};
use kraai_agent::AgentManager;
use kraai_persistence::Persistence;

use crate::benchmark::{Benchmark, Context};
use crate::fixtures::{agent, provider};

pub(super) struct Case;

impl Benchmark for Case {
    type Fixture = ();
    type Output = (AgentManager, Persistence, String);

    const NAME: &str = "agent-manager-startup";
    const OPERATIONS: u64 = 1;
    const DESCRIPTION: &str =
        "Open a fresh database, initialize the agent manager and profiles, and create a session";
    const FIXTURES: &[&[u8]] = agent::SOURCES;

    async fn setup(context: &Context) -> Result<Self::Fixture> {
        agent::prepare(&context.directory)
    }

    async fn run(context: &Context, _fixture: &mut Self::Fixture) -> Result<Self::Output> {
        let (mut manager, persistence) = agent::create(&context.directory).await?;
        let session = manager
            .create_session_with(None, Some(agent::PROFILE.into()))
            .await?;
        Ok((manager, persistence, session))
    }

    async fn verify(
        _context: &Context,
        _fixture: Self::Fixture,
        output: Self::Output,
    ) -> Result<()> {
        let (manager, _persistence, session) = output;
        let profiles = manager.list_agent_profiles(&session).await?;
        ensure!(
            profiles.warnings.is_empty(),
            "Profile initialization produced warnings"
        );
        ensure!(
            profiles.selected_profile_id.as_deref() == Some(agent::PROFILE),
            "Startup selected the wrong profile"
        );
        let models = manager.list_models().await;
        ensure!(
            models.get(&provider::provider_id()).is_some_and(|models| {
                models.len() == 1
                    && models
                        .first()
                        .is_some_and(|model| model.id == provider::model_id())
            }),
            "Offline model initialization failed"
        );
        ensure!(
            manager.list_sessions().await?.len() == 1,
            "Startup did not persist its session"
        );
        ensure!(
            manager.get_chat_history(&session).await?.is_empty(),
            "Fresh session has unexpected history"
        );
        Ok(())
    }
}
