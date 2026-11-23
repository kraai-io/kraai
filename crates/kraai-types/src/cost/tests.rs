use super::*;

#[test]
fn prices_each_token_category_once() {
    let rates = TokenRates {
        input: Usd(2_000_000_000),
        output: Usd(8_000_000_000),
        cache_read: Some(Usd(500_000_000)),
        cache_write: Some(Usd(3_000_000_000)),
        reasoning: None,
    };
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: 10,
        cache_read_tokens: 40,
        cache_write_tokens: 30,
        total_tokens: 200,
        cost: None,
    };
    assert_eq!(rates.estimate(&usage), Some(Usd(550_000)));
    let mut missing_cache_rate = rates;
    missing_cache_rate.cache_write = None;
    assert_eq!(missing_cache_rate.estimate(&usage), None);
}

#[test]
fn validates_money_and_preserves_small_charges() {
    for value in [-1.0, f64::INFINITY, f64::NAN, f64::MAX] {
        assert_eq!(Usd::from_dollars(value), None);
    }
    assert_eq!(Usd::from_dollars(0.0), Some(Usd(0)));
    assert_eq!(Usd(1).to_string(), "<$0.0001");
    assert_eq!(Usd(123_450_000).to_string(), "$0.1235");
}

#[test]
fn summary_formats_known_unknown_upstream_and_overflow_costs() {
    for (reported, unknown, upstream, estimated, overflow, expected) in [
        (0, 0, 0, false, false, "$0.0000"),
        (0, 0, 1, false, false, "$0.0000 + ~<$0.0001 upstream"),
        (0, 1, 0, true, false, "$unknown"),
        (
            0,
            1,
            50_000_000,
            true,
            false,
            "$unknown + ~$0.0500 upstream",
        ),
        (1, 0, 0, false, false, "$0.0000"),
        (
            1,
            0,
            50_000_000,
            true,
            false,
            "~$0.0000 + ~$0.0500 upstream",
        ),
        (
            1,
            1,
            50_000_000,
            false,
            false,
            "$0.0000 + $unknown + ~$0.0500 upstream",
        ),
        (1, 1, 50_000_000, true, true, "cost overflow"),
    ] {
        let summary = CostSummary {
            reported,
            unknown,
            upstream: Usd(upstream),
            estimated,
            overflow,
            ..Default::default()
        };
        assert_eq!(summary.to_string(), expected);
        assert_eq!(format!("{summary:>80.1}"), expected);
    }
}

#[test]
fn summary_includes_subscription_estimates_and_unknown_requests() {
    let mut request = RequestUsage {
        message_id: MessageId::new("request"),
        provider_id: ProviderId::new("provider"),
        model_id: ModelId::new("model"),
        started_at: 0,
        subscription: false,
        unpriced_attempts: 0,
        usage: None,
    };
    let mut summary = CostSummary::default();
    summary.add(&request);
    assert_eq!(summary.to_string(), "$unknown");
    request.usage = Some(TokenUsage {
        cost: Some(RequestCost {
            amount: Usd(0),
            source: "provider".into(),
            rates: None,
            priced_at: 0,
            upstream: None,
        }),
        ..Default::default()
    });
    summary.add(&request);
    request.subscription = true;
    summary.add(&request);
    assert_eq!(summary.to_string(), "~$0.0000 + $unknown");
    if let Some(usage) = &mut request.usage
        && let Some(cost) = &mut usage.cost
    {
        cost.amount = Usd(50_000_000);
    }
    summary.add(&request);
    assert_eq!(summary.to_string(), "~$0.0500 + $unknown");
}

#[test]
fn summary_keeps_upstream_estimates_separate_and_counts_unpriced_attempts() {
    let mut request = RequestUsage {
        message_id: MessageId::new("request"),
        provider_id: ProviderId::new("openrouter"),
        model_id: ModelId::new("model"),
        started_at: 0,
        subscription: false,
        unpriced_attempts: 2,
        usage: Some(TokenUsage {
            cost: Some(RequestCost {
                amount: Usd(0),
                source: "openrouter".into(),
                rates: None,
                priced_at: 0,
                upstream: Some(Usd(40_000_000)),
            }),
            ..Default::default()
        }),
    };
    let mut summary = CostSummary::default();
    summary.add(&request);
    assert_eq!(summary.amount, Usd(0));
    assert_eq!(
        summary.to_string(),
        "$0.0000 + $unknown + ~$0.0400 upstream"
    );
    request.usage = None;
    summary.add(&request);
    assert_eq!(summary.unknown, 5);
    request.subscription = true;
    summary.add(&request);
    assert_eq!(summary.unknown, 8);
    assert_eq!(summary.subscriptions, 1);
}
