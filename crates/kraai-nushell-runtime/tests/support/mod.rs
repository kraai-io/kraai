use std::path::{Path, PathBuf};
use std::time::Duration;

use kraai_execution_types::{SandboxCapabilities, SandboxCapability, ScriptExecutionId};
use kraai_nushell_runtime::ScriptExecutionPlan;
use kraai_sandbox::PrivateTempConfig;
use kraai_test_support::{HANG_TIMEOUT, hang_guard};
use ulid::Ulid;

pub struct FakeHost {
    pub plan: ScriptExecutionPlan,
    pub socket: PathBuf,
}

impl FakeHost {
    pub fn new(timeout: Duration, source: Vec<u8>) -> Self {
        let private_temp = PrivateTempConfig::default().reserve().unwrap();
        let socket = private_temp
            .path()
            .expect("missing private temp")
            .join("host.sock");
        let path = std::env::var("PATH").unwrap();
        let shell = std::env::split_paths(&path)
            .map(|directory| directory.join("sh"))
            .find(|candidate| candidate.is_file())
            .expect("missing test shell");
        let mut plan = ScriptExecutionPlan::new(
            ScriptExecutionId::new(Ulid::generate()),
            shell,
            source,
            std::env::current_dir().unwrap(),
            SandboxCapabilities::new([SandboxCapability::NoSandbox]),
            timeout,
        );
        plan.host_arguments = vec![
            "-c".into(),
            format!("exec sleep {}", (HANG_TIMEOUT * 2).as_secs()).into(),
        ];
        plan.environment.insert(String::from("PATH"), path);
        plan.private_temp = private_temp;
        Self { plan, socket }
    }
}

pub async fn connect(path: &Path) -> tokio::net::UnixStream {
    hang_guard(async {
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
    .await
    .unwrap()
    .unwrap()
}
