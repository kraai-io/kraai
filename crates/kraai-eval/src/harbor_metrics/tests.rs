use super::*;

#[test]
fn proxy_cache_writes_contribute_to_total_input_without_becoming_uncached_input() {
    let metrics = trial_metrics(
        &Value::Null,
        Some(ProxyMetrics {
            requests: 1,
            usage: crate::UsageMetrics {
                total_tokens: 70,
                input_tokens: 10,
                cache_read_tokens: 20,
                cache_write_tokens: 30,
                output_tokens: 4,
                reasoning_tokens: 6,
            },
            ..Default::default()
        }),
        false,
    )
    .unwrap();
    assert!(metrics.input_tokens == Some(60));
    assert!(metrics.uncached_input_tokens == Some(10));
    assert!(metrics.cache_read_tokens == Some(20));
    assert!(metrics.output_tokens == Some(10));
    assert!(metrics.reasoning_tokens == Some(6));
    assert!(metrics.total_tokens == Some(70));
}

#[test]
fn incomplete_request_accounting_preserves_known_usage_without_pairing_partial_totals() {
    for (samples, unrecorded, error) in [(1, 0, None), (2, 1, None), (2, 0, Some("failed"))] {
        let metrics = trial_metrics(
            &Value::Null,
            Some(ProxyMetrics {
                requests: 2,
                accounting_error: error.map(str::to_owned),
                usage: crate::UsageMetrics {
                    total_tokens: 15,
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
                accounting: Some(crate::RequestAccounting {
                    model_requests: 2,
                    unrecorded_requests: unrecorded,
                    context: crate::ContextMetrics {
                        samples,
                        total_input_tokens: 10,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                ..Default::default()
            }),
            false,
        )
        .unwrap();
        assert!(metrics.total_tokens.is_none());
        assert!(metrics.input_tokens.is_none());
        assert!(metrics.output_tokens.is_none());
        assert!(metrics.proxy_requests == Some(2));
        assert!(metrics.usage_source == "controller_proxy_usage_incomplete");
        assert!(metrics.accounting.as_ref().is_some_and(|accounting| {
            accounting.context.samples == samples && accounting.context.total_input_tokens == 10
        }));
    }
}

#[test]
fn unrecorded_requests_without_accounting_never_supply_complete_token_totals() {
    let metrics = trial_metrics(
        &Value::Null,
        Some(ProxyMetrics {
            requests: 1,
            unrecorded_requests: 1,
            usage: crate::UsageMetrics {
                input_tokens: 10,
                total_tokens: 10,
                ..Default::default()
            },
            ..Default::default()
        }),
        false,
    )
    .unwrap();
    assert!(metrics.total_tokens.is_none());
}
