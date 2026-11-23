use super::*;

#[test]
fn proxy_usage_is_authoritative_when_both_sources_exist() {
    let proxy_usage = UsageMetrics {
        total_tokens: 10,
        ..UsageMetrics::default()
    };
    let harness_usage = UsageMetrics {
        total_tokens: 20,
        ..UsageMetrics::default()
    };
    let metrics = EvaluationMetrics {
        proxy: Some(ProxyMetrics {
            usage: proxy_usage.clone(),
            ..ProxyMetrics::default()
        }),
        harness: Some(HarnessMetrics {
            request_costs: std::collections::BTreeMap::new(),
            request_costs_complete: false,
            schema_version: 1,
            turns: None,
            script_executions: None,
            final_context_tokens: None,
            usage: Some(harness_usage),
        }),
    };
    assert_eq!(metrics.usage(), Some(&proxy_usage));
}

#[test]
fn proxy_metrics_default_client_disconnects_when_loading_older_results() {
    let metrics: ProxyMetrics = serde_json::from_str(
        r#"{"requests":1,"successful_requests":1,"failed_requests":0,"duration_ms":2,"usage":{"total_tokens":3,"input_tokens":2,"output_tokens":1,"reasoning_tokens":0,"cache_read_tokens":0}}"#,
    ).unwrap();

    assert!(metrics.client_disconnects == 0);
}

#[test]
fn loads_native_request_costs_keyed_by_message_id() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-native-metrics-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("harness-metrics.json");
    let emitted = serde_json::json!({
        "schema_version": 1,
        "turns": 1,
        "script_executions": 1,
        "final_context_tokens": 2243,
        "usage": {
            "total_tokens": 2243, "input_tokens": 2108, "output_tokens": 135,
            "reasoning_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0
        },
        "request_costs": {
            "request": {
                "message_id": "request", "provider_id": "openai-codex",
                "model_id": "gpt-5.6-sol-low", "started_at": 1789315143614_u64,
                "subscription": true, "unpriced_attempts": 0,
                "usage": {
                    "total_tokens": 2243, "input_tokens": 2108, "output_tokens": 135,
                    "reasoning_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0,
                    "cost": {
                        "amount": 11132000, "source": "models.dev/openai/gpt-5.6-sol",
                        "priced_at": 1789315143, "upstream": null,
                        "rates": {
                            "input": 4000000000_u64, "output": 20000000000_u64,
                            "cache_read": 400000000, "cache_write": 5000000000_u64,
                            "reasoning": null
                        }
                    }
                }
            }
        },
        "request_costs_complete": true
    });
    fs::write(&path, serde_json::to_vec(&emitted).unwrap()).unwrap();
    let metrics = load_harness_metrics(&path)
        .unwrap()
        .expect("native metrics were discarded");
    assert!(metrics.request_costs.len() == 1);
    assert!(metrics.request_costs_complete);
    assert!(serde_json::to_value(&metrics).unwrap() == emitted);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_sparse_metrics_past_the_limit_without_reading_them() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-sparse-metrics-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("harness-metrics.json");
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_HARNESS_METRICS_BYTES + 1)
        .unwrap();
    let error = load_harness_metrics(&path).expect_err("oversized metrics were accepted");
    fs::remove_dir_all(root).unwrap();
    assert!(matches!(
        error.downcast_ref::<ReadLimitError>(),
        Some(ReadLimitError::Exceeded {
            limit: MAX_HARNESS_METRICS_BYTES
        })
    ));
}
