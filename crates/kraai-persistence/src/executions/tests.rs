use super::*;
use kraai_execution_types::SandboxCapability;
use std::path::PathBuf;
use tokio::fs;

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
        rewrites: ScriptRewrites::default(),
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
        coalesce_output(vec![
            (ScriptOutputStream::Stdout, b"a".to_vec()),
            (ScriptOutputStream::Stdout, b"b".to_vec()),
            (ScriptOutputStream::Stderr, Vec::new()),
            (ScriptOutputStream::Stdout, b"c".to_vec()),
        ]),
        vec![(ScriptOutputStream::Stdout, b"abc".to_vec())]
    );
    let _ = fs::remove_dir_all(data_dir).await;
}

#[tokio::test]
async fn records_from_before_merged_calls_keep_their_bash_rewrites() {
    let data_dir = test_dir("rewrite-records");
    let store = SqliteScriptExecutionStore::new(&data_dir);
    let id = ScriptExecutionId::new(Ulid::generate());
    let record = store.create(execution(&id)).await.unwrap();
    let mut json = serde_json::to_value(&record).unwrap();
    let object = json.as_object_mut().unwrap();
    object.remove("merged_calls");
    object.remove("dropped_calls");
    object.insert(
        String::from("rewrites"),
        serde_json::json!(["`find` → `^find`"]),
    );
    let old: ScriptExecutionRecord = serde_json::from_value(json).unwrap();
    assert_eq!(old.rewrites.bash, [String::from("`find` → `^find`")]);
    assert_eq!(old.rewrites.merged_calls, 0);
    fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn output_appends_leave_the_record_alone_and_require_a_running_execution() {
    let data_dir = test_dir("output-record");
    let id = ScriptExecutionId::new(Ulid::generate());
    let store = SqliteScriptExecutionStore::new(&data_dir);
    store.create(execution(&id)).await.unwrap();
    let chunk = || vec![(ScriptOutputStream::Stdout, b"out".to_vec())];
    assert!(store.append_output(&id, chunk()).await.is_err());
    let running = store.mark_running(&id).await.unwrap();
    store.append_output(&id, chunk()).await.unwrap();
    assert_eq!(store.get(&id).await.unwrap(), Some(running));
    assert!(
        store
            .append_output(&ScriptExecutionId::new(Ulid::generate()), chunk())
            .await
            .is_err()
    );
    store
        .finish(
            &id,
            ScriptExecutionCompletion {
                status: ScriptExecutionStatus::Completed,
                exit_code: Some(0),
                sandbox_denied: false,
                error: None,
                stdout: b"out".to_vec(),
                stderr: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert!(store.append_output(&id, chunk()).await.is_err());
    assert_eq!(store.read_output(&id).await.unwrap().stdout, b"out");
    fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn settled_executions_leave_the_recovery_list_without_changing_their_timing() {
    let data_dir = test_dir("settled");
    let store = SqliteScriptExecutionStore::new(&data_dir);
    let settled = ScriptExecutionId::new(Ulid::generate());
    let pending = ScriptExecutionId::new(Ulid::generate());
    store.create(execution(&settled)).await.unwrap();
    store.create(execution(&pending)).await.unwrap();
    let created = store.get(&settled).await.unwrap().unwrap();
    store.mark_settled(&settled).await.unwrap();
    store.mark_settled(&settled).await.unwrap();
    let unsettled = store
        .list_unsettled_for_session(&SessionId::new("session"))
        .await
        .unwrap();
    assert_eq!(
        unsettled
            .iter()
            .map(|record| &record.id)
            .collect::<Vec<_>>(),
        vec![&pending]
    );
    let reopened = SqliteScriptExecutionStore::new(&data_dir)
        .get(&settled)
        .await
        .unwrap()
        .unwrap();
    assert!(reopened.settled);
    assert_eq!(reopened.updated_at_millis, created.updated_at_millis);
    fs::remove_dir_all(data_dir).await.unwrap();
}
