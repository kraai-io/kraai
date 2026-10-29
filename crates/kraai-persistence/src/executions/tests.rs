use super::*;
use kraai_execution_types::SandboxCapability;
use kraai_types::ToolCallId;
use std::path::PathBuf;
use tokio::fs;
use ulid::Ulid;

fn test_dir(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("kraai-executions-{name}-{}", Ulid::generate()));
    std::fs::create_dir(&directory).unwrap();
    directory
}

fn execution(id: &ScriptExecutionId) -> NewScriptExecution {
    NewScriptExecution {
        id: id.clone(),
        session_id: SessionId::new("session"),
        source_message_id: MessageId::new("message"),
        call_id: ToolCallId::new("call-1"),
        profile: ScriptProfileSnapshot {
            id: String::from("coding"),
            commands: Vec::new(),
            permissions: kraai_execution_types::SandboxPermissionSet::new([
                SandboxCapability::WorkspaceRead,
            ]),
            permission_rules: kraai_execution_types::CapabilityPermissionRules::default(),
            escalation_policy: kraai_execution_types::EscalationPolicy::Prompt,
            environment: kraai_execution_types::EnvironmentPolicy::AllowList,
            nushell_startup: kraai_execution_types::NushellStartup::Clean,
            path: kraai_execution_types::PathPolicy::Inherit,
        },
        source: b"1 + 1".to_vec(),
        requested_capabilities: SandboxCapabilities::default(),
        effective_capabilities: SandboxCapabilities::new([SandboxCapability::WorkspaceRead]),
        timeout: Some(Duration::from_secs(10)),
    }
}

#[tokio::test]
async fn execution_timing_starts_at_running_and_survives_reopen() {
    let data_dir = test_dir("execution-timing");
    let store = SqliteScriptExecutionStore::new(&data_dir);
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
        let reopened = SqliteScriptExecutionStore::new(&data_dir);
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
async fn output_is_written_before_terminal_record_is_exposed() {
    let data_dir = test_dir("terminal-output");
    let id = ScriptExecutionId::new(Ulid::generate());
    let store = SqliteScriptExecutionStore::new(&data_dir);
    store.create(execution(&id)).await.unwrap();
    store.mark_running(&id).await.unwrap();
    store
        .append_output(&id, vec![(ScriptOutputStream::Stdout, b"partial".to_vec())])
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

    let reopened = SqliteScriptExecutionStore::new(&data_dir);
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
    let store = SqliteScriptExecutionStore::new(&data_dir);
    let id = ScriptExecutionId::new(Ulid::generate());
    store.create(execution(&id)).await.unwrap();
    let image = kraai_execution_types::ImageAttachment {
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
        .append_output(
            &id,
            vec![(ScriptOutputStream::Stdout, b"later output".to_vec())],
        )
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
    let reopened = SqliteScriptExecutionStore::new(&data_dir);
    assert_eq!(
        reopened.get(&id).await.unwrap().unwrap().images,
        finished.images
    );
    assert!(reopened.append_image(&id, 2, image).await.is_err());
    fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn batched_output_keeps_each_stream_in_order() {
    let data_dir = test_dir("batched-output");
    let id = ScriptExecutionId::new(Ulid::generate());
    let store = SqliteScriptExecutionStore::new(&data_dir);
    store.create(execution(&id)).await.unwrap();
    store.mark_running(&id).await.unwrap();
    store
        .append_output(
            &id,
            vec![
                (ScriptOutputStream::Stdout, b"a".to_vec()),
                (ScriptOutputStream::Stdout, b"b".to_vec()),
                (ScriptOutputStream::Stderr, Vec::new()),
                (ScriptOutputStream::Stderr, b"x".to_vec()),
                (ScriptOutputStream::Stdout, b"c".to_vec()),
            ],
        )
        .await
        .unwrap();
    store
        .append_output(&id, vec![(ScriptOutputStream::Stderr, b"y".to_vec())])
        .await
        .unwrap();
    let output = store.read_output(&id).await.unwrap();
    assert_eq!(output.stdout, b"abc");
    assert_eq!(output.stderr, b"xy");
    assert_eq!(
        super::coalesce_output(vec![
            (ScriptOutputStream::Stdout, b"a".to_vec()),
            (ScriptOutputStream::Stdout, b"b".to_vec()),
            (ScriptOutputStream::Stderr, Vec::new()),
            (ScriptOutputStream::Stdout, b"c".to_vec()),
        ]),
        vec![(ScriptOutputStream::Stdout, b"abc".to_vec())]
    );
    let _ = fs::remove_dir_all(data_dir).await;
}
