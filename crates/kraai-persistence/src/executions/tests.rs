use super::*;
use kraai_types::{SandboxCapability, ToolCallId};
use ulid::Ulid;

fn test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kraai-executions-{name}-{}", Ulid::generate()))
}

fn execution(id: &ScriptExecutionId) -> NewScriptExecution {
    NewScriptExecution {
        id: id.clone(),
        session_id: String::from("session"),
        source_message_id: MessageId::new("message"),
        call_id: ToolCallId::new("call-1"),
        profile: ScriptProfileSnapshot {
            id: String::from("coding"),
            commands: Vec::new(),
            permissions: kraai_types::SandboxPermissionSet::new([SandboxCapability::WorkspaceRead])
                .unwrap(),
            permission_rules: kraai_types::CapabilityPermissionRules::default(),
            escalation_policy: kraai_types::EscalationPolicy::Prompt,
            environment: kraai_types::EnvironmentPolicy::AllowList,
            nushell_startup: kraai_types::NushellStartup::Clean,
            path: kraai_types::PathPolicy::Inherit,
        },
        source: b"1 + 1".to_vec(),
        requested_capabilities: SandboxCapabilities::default(),
        effective_capabilities: SandboxCapabilities::new([SandboxCapability::WorkspaceRead])
            .unwrap(),
        timeout: Some(Duration::from_secs(10)),
    }
}

#[tokio::test]
async fn cancelled_creation_finishes_before_a_waiting_phase_transition() {
    let data_dir = test_dir("cancelled-create");
    let id = ScriptExecutionId::new(Ulid::generate());
    let store = FileScriptExecutionStore::new(&data_dir);
    let mut create = Box::pin(store.create(execution(&id)));
    assert!(
        create
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    drop(create);
    assert!(!data_dir.exists());

    let mut running = Box::pin(store.mark_running(&id));
    assert!(
        running
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
    let record = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.phase, ScriptExecutionPhase::Running);
    assert_eq!(store.read_source(&id).await.unwrap(), b"1 + 1");
    store
        .append_output(&id, ScriptOutputStream::Stdout, b"complete".to_vec())
        .await
        .unwrap();
    assert_eq!(store.read_output(&id).await.unwrap().stdout, b"complete");
    fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn execution_timing_starts_at_running_and_survives_reopen() {
    let data_dir = test_dir("execution-timing");
    let store = FileScriptExecutionStore::new(&data_dir);
    for run in [false, true] {
        let id = ScriptExecutionId::new(Ulid::generate());
        let prepared = store.create(execution(&id)).await.unwrap();
        assert_eq!(prepared.started_at_millis, None);
        assert_eq!(prepared.elapsed_millis(), None);
        let waiting = store.mark_awaiting_approval(&id).await.unwrap();
        assert_eq!(waiting.started_at_millis, None);
        let started = if run {
            let running = store.mark_running(&id).await.unwrap();
            assert_eq!(running.started_at_millis, Some(running.updated_at_millis));
            assert!(store.mark_running(&id).await.is_err());
            running.started_at_millis
        } else {
            None
        };
        store
            .finish(
                &id,
                ScriptExecutionCompletion {
                    status: if run {
                        ScriptExecutionStatus::Completed
                    } else {
                        ScriptExecutionStatus::Denied
                    },
                    exit_code: run.then_some(0),
                    sandbox_denied: false,
                    error: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            )
            .await
            .unwrap();
        let reopened = FileScriptExecutionStore::new(&data_dir);
        let mut record = reopened.get(&id).await.unwrap().unwrap();
        assert_eq!(record.started_at_millis, started);
        record.created_at_millis = 1;
        if let Some(started) = started {
            record.updated_at_millis = started + 125;
            assert_eq!(record.elapsed_millis(), Some(125));
            record.updated_at_millis = started.saturating_sub(1);
            assert_eq!(record.elapsed_millis(), Some(0));
        } else {
            assert_eq!(record.elapsed_millis(), None);
        }
    }
    let _ = fs::remove_dir_all(data_dir).await;
}

#[tokio::test]
async fn output_reads_wait_for_both_streams_to_finish_replacement() {
    let data_dir = test_dir("output-read-lock");
    let id = ScriptExecutionId::new(Ulid::generate());
    let store = FileScriptExecutionStore::new(&data_dir);
    store.create(execution(&id)).await.unwrap();
    let execution_dir = store.execution_dir(&id).unwrap();
    let guard = store.execution_locks.lock(&id).await;
    atomic_write(&execution_dir.join(STDOUT_FILE), b"final stdout")
        .await
        .unwrap();

    let mut output = Box::pin(store.read_output(&id));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), output.as_mut())
            .await
            .is_err()
    );

    atomic_write(&execution_dir.join(STDERR_FILE), b"final stderr")
        .await
        .unwrap();
    drop(guard);
    let output = output.await.unwrap();
    assert_eq!(output.stdout, b"final stdout");
    assert_eq!(output.stderr, b"final stderr");
    fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn output_is_written_before_terminal_record_is_exposed() {
    let data_dir = test_dir("terminal-output");
    let id = ScriptExecutionId::new(Ulid::generate());
    let store = FileScriptExecutionStore::new(&data_dir);
    store.create(execution(&id)).await.unwrap();
    store.mark_running(&id).await.unwrap();
    store
        .append_output(&id, ScriptOutputStream::Stdout, b"partial".to_vec())
        .await
        .unwrap();
    let prefix = store.read_output(&id).await.unwrap();
    assert_eq!(prefix.stdout, b"partial");
    store
        .finish(
            &id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(0),
                sandbox_denied: false,
                error: None,
                stdout: b"ok\0binary".to_vec(),
                stderr: b"warning".to_vec(),
            },
        )
        .await
        .unwrap();

    let reopened = FileScriptExecutionStore::new(&data_dir);
    let record = reopened.get(&id).await.unwrap().unwrap();
    let output = reopened.read_output(&id).await.unwrap();
    assert_eq!(record.phase, ScriptExecutionPhase::Finished);
    assert_eq!(record.status, Some(ScriptExecutionStatus::Completed));
    assert_eq!(output.stdout, b"ok\0binary");
    assert_eq!(output.stderr, b"warning");
    let _ = fs::remove_dir_all(data_dir).await;
}

#[tokio::test]
async fn image_results_survive_failure_and_reopen_with_idempotent_sequences() {
    let data_dir = test_dir("image-results");
    let store = FileScriptExecutionStore::new(&data_dir);
    let id = ScriptExecutionId::new(Ulid::generate());
    store.create(execution(&id)).await.unwrap();
    let image = kraai_types::ImageAttachment {
        id: "a".repeat(64),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
        byte_length: 100,
    };
    assert!(store.append_image(&id, 1, image.clone()).await.is_err());
    store.mark_running(&id).await.unwrap();
    assert!(store.append_image(&id, 0, image.clone()).await.is_err());
    store.append_image(&id, 1, image.clone()).await.unwrap();
    store.append_image(&id, 1, image.clone()).await.unwrap();
    let mut other = image.clone();
    other.id = "b".repeat(64);
    assert!(store.append_image(&id, 1, other).await.is_err());
    store
        .append_output(&id, ScriptOutputStream::Stdout, b"later output".to_vec())
        .await
        .unwrap();
    let finished = store
        .finish(
            &id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(1),
                sandbox_denied: false,
                error: Some("failed after viewing image".into()),
                stdout: b"later output".to_vec(),
                stderr: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(finished.images.values().collect::<Vec<_>>(), vec![&image]);
    let reopened = FileScriptExecutionStore::new(&data_dir);
    assert_eq!(
        reopened.get(&id).await.unwrap().unwrap().images,
        finished.images
    );
    assert!(reopened.append_image(&id, 2, image).await.is_err());
    fs::remove_dir_all(data_dir).await.unwrap();
}
