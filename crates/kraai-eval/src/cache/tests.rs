use super::*;

#[test]
fn bundle_checksum_uses_standard_sha256() {
    let path = std::env::temp_dir().join(format!("kraai-sha256-{}", ulid::Ulid::generate()));
    fs::write(&path, b"abc").unwrap();
    let digest = sha256_file(&path).unwrap();
    let cached = hash_file(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert!(digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert!(digest != cached);
}

#[test]
fn streamed_file_hash_preserves_length_prefixed_identity() {
    let root = std::env::temp_dir().join(format!("kraai-streamed-hash-{}", ulid::Ulid::generate()));
    fs::create_dir(&root).unwrap();
    let path = root.join("artifact");
    for length in [0, 1, 8191, 8192, 8193, 32769] {
        let bytes = (0..length)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(&path, &bytes).unwrap();
        assert!(hash_file(&path).unwrap() == hash_chunks(&[bytes.as_slice()]));
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn hashing_supports_regular_files_with_virtual_metadata_lengths() {
    let path = Path::new("/proc/sys/kernel/pid_max");
    if path.exists() {
        let bytes = fs::read(path).unwrap();
        assert!(hash_file(path).unwrap() == hash_chunks(&[bytes.as_slice()]));
    }
}

#[test]
fn hashed_text_retains_the_bytes_from_its_single_read() {
    let root = std::env::temp_dir().join(format!("kraai-hashed-text-{}", ulid::Ulid::generate()));
    fs::create_dir(&root).unwrap();
    let path = root.join("events");
    let original = "{\"message\":\"é🦀\"}\r\n\n";
    fs::write(&path, original).unwrap();
    let (text, digest) = read_hashed_text_file(&path).unwrap();
    fs::write(&path, "replacement").unwrap();
    assert!(text == original);
    assert!(digest == hash_chunks(&[original.as_bytes()]));
    fs::remove_dir_all(root).unwrap();
}

struct AccountingFixture {
    root: PathBuf,
    store: ResultStore,
}

impl AccountingFixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("kraai-cache-accounting-{}", ulid::Ulid::generate()));
        let store = ResultStore::new(
            &root,
            &root.join(".cache/staging"),
            Path::new("task/attempt-1"),
        );
        fs::create_dir_all(&store.final_dir).unwrap();
        let fixture = Self { root, store };
        fs::write(
            fixture.store.final_dir.join("proxy.events.jsonl"),
            serde_json::to_vec(&serde_json::json!({
                "method": "POST", "path": "/v1/responses", "model": "model",
                "usage": {"total_tokens": 1_000_000, "input_tokens": 1_000_000,
                "output_tokens": 0, "reasoning_tokens": 0, "cache_read_tokens": 0}
            }))
            .unwrap(),
        )
        .unwrap();
        let proxy = crate::ProxyMetrics {
            requests: 1,
            ..Default::default()
        };
        let result: RunResult = serde_json::from_value(serde_json::json!({
            "schema_version": 6, "experiment_id": "experiment",
            "artifact_path": fixture.store.relative_dir(), "task_id": "task",
            "harness_name": "harness", "model_label": "model", "attempt": 0,
            "runner_version": "version", "runner_artifact_sha256": "runner",
            "task_sha256": "task", "grader_sha256": "grader", "status": "passed",
            "sandbox": {"backend": "test", "network": "disabled",
            "environment_cleared": true, "max_memory_bytes": 1,
            "max_processes": 1, "cpu_quota_percent": 100},
            "graders": [], "started_at_ms": 1, "completed_at_ms": 2,
            "duration_ms": 1, "metrics": {"proxy": proxy}
        }))
        .unwrap();
        fs::write(
            fixture.store.final_dir.join("result.json"),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        fixture
    }
}

impl Drop for AccountingFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cached_and_direct_loads_use_saved_accounting_and_reject_changed_events() {
    let fixture = AccountingFixture::new();
    let path = fixture.store.final_dir.join("result.json");
    let original = fs::read(&path).unwrap();
    let accounting =
        crate::analyze_requests(&path.with_file_name("proxy.events.jsonl"), 1).unwrap();
    fs::write(
        path.with_file_name("request-accounting.json"),
        serde_json::to_vec(&accounting).unwrap(),
    )
    .unwrap();
    let requests = |result: &RunResult| {
        result
            .metrics
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.accounting.as_ref())
            .map(|accounting| accounting.model_requests)
    };
    let cached = fixture
        .store
        .load_result()
        .unwrap()
        .expect("missing cached run");
    assert!(requests(&cached) == Some(1));
    assert!(requests(&load_run_result(&path).unwrap()) == Some(1));
    assert!(fs::read(&path).unwrap() == original);
    fs::write(path.with_file_name("proxy.events.jsonl"), "").unwrap();
    assert!(fixture.store.load_result().is_err() && load_run_result(&path).is_err());
}
