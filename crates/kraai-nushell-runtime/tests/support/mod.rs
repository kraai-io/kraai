use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_nushell_runtime::ScriptExecutionPlan;
use kraai_sandbox::PrivateTempConfig;
use kraai_types::{SandboxCapabilities, SandboxCapability, ScriptExecutionId};
use ulid::Ulid;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct FakeHost {
    pub plan: ScriptExecutionPlan,
    pub socket: PathBuf,
}

impl FakeHost {
    pub fn new(timeout: Duration, source: Vec<u8>) -> TestResult<Self> {
        let private_temp = PrivateTempConfig::default().reserve()?;
        let socket = private_temp
            .path()
            .ok_or("missing private temp")?
            .join("host.sock");
        let path = std::env::var("PATH")?;
        let shell = std::env::split_paths(&path)
            .map(|directory| directory.join("sh"))
            .find(|candidate| candidate.is_file())
            .ok_or("missing test shell")?;
        let mut plan = ScriptExecutionPlan::new(
            ScriptExecutionId::new(Ulid::generate()),
            shell,
            source,
            std::env::current_dir()?,
            SandboxCapabilities::new([SandboxCapability::NoSandbox])?,
            timeout,
        );
        plan.host_arguments = ["-c", "exec sleep 30"]
            .into_iter()
            .map(Into::into)
            .collect();
        plan.environment.insert(String::from("PATH"), path);
        plan.private_temp = private_temp;
        Ok(Self { plan, socket })
    }
}

pub async fn connect(path: &Path) -> TestResult<tokio::net::UnixStream> {
    Ok(tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match tokio::net::UnixStream::connect(path).await {
                Ok(stream) => return Ok::<_, std::io::Error>(stream),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await??)
}
