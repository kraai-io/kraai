use super::*;
use crate::{DynamicConfig, DynamicValue, ModelConfig, ProviderConfig};
use kraai_types::Usd;
use tokio_util::sync::CancellationToken;

#[test]
fn model_index_preserves_provider_order_and_duplicate_rate_precedence() {
    let provider = |id: &str, kind: &str| ProviderConfig {
        id: ProviderId::new(id),
        type_id: kind.to_owned(),
        config: DynamicConfig::new(),
    };
    let model = |provider: &str, input: Option<&str>| ModelConfig {
        options: Vec::new(),
        remove_options: Vec::new(),
        id: ModelId::new("model"),
        provider_id: ProviderId::new(provider),
        config: input.map_or_else(DynamicConfig::new, |input| {
            DynamicConfig::from([
                (String::from("price_input"), DynamicValue::from(input)),
                (String::from("price_output"), DynamicValue::from("8")),
            ])
        }),
    };
    let config = ProviderManagerConfig {
        providers: vec![
            provider("b", "first"),
            provider("a", "second"),
            provider("b", "last"),
            provider("empty", "empty"),
        ],
        models: vec![
            model("a", Some("2")),
            model("b", Some("3")),
            model("b", None),
            model("orphan", Some("invalid")),
            model("a", Some("4")),
        ],
    };
    for providers in [
        config.providers.clone(),
        vec![provider("b", "single")],
        Vec::new(),
    ] {
        let current = ProviderManagerConfig {
            providers,
            models: config.models.clone(),
        };
        let calls = std::cell::RefCell::new(Vec::new());
        let actual = Pricing::new(&current, |kind| {
            calls.borrow_mut().push(kind.to_owned());
            ProviderPricingPolicy {
                subscription: kind == "last",
                ..ProviderPricingPolicy::default()
            }
        })
        .unwrap();
        assert!(
            *calls.borrow()
                == current
                    .providers
                    .iter()
                    .map(|provider| provider.type_id.clone())
                    .collect::<Vec<_>>()
        );
        let expected = current
            .providers
            .iter()
            .map(|provider| {
                Ok((
                    provider.id.clone(),
                    PricingConfig::new(
                        provider,
                        &current.models,
                        ProviderPricingPolicy {
                            subscription: provider.type_id == "last",
                            ..ProviderPricingPolicy::default()
                        },
                    )?,
                ))
            })
            .collect::<color_eyre::Result<BTreeMap<_, _>>>()
            .unwrap();
        assert!(actual.configs.len() == expected.len());
        for (id, expected) in expected {
            let actual = actual
                .configs
                .get(&id)
                .unwrap_or_else(|| panic!("missing provider {id}"));
            assert!(actual.models == expected.models);
            assert!(actual.subscription == expected.subscription);
            assert!(actual.api == expected.api && actual.provider == expected.provider);
        }
    }
}

#[test]
fn model_index_preserves_first_validation_error_and_policy_callback_order() {
    let config = ProviderManagerConfig {
        providers: ["b", "a"]
            .map(|id| ProviderConfig {
                id: ProviderId::new(id),
                type_id: id.to_owned(),
                config: DynamicConfig::new(),
            })
            .to_vec(),
        models: [("a", "bad", "1"), ("b", "1", "bad"), ("b", "bad", "1")]
            .map(|(provider, input, output)| ModelConfig {
                options: Vec::new(),
                remove_options: Vec::new(),
                id: ModelId::new("model"),
                provider_id: ProviderId::new(provider),
                config: DynamicConfig::from([
                    (String::from("price_input"), DynamicValue::from(input)),
                    (String::from("price_output"), DynamicValue::from(output)),
                ]),
            })
            .to_vec(),
    };
    let calls = std::cell::RefCell::new(Vec::new());
    let error = Pricing::new(&config, |kind| {
        calls.borrow_mut().push(kind.to_owned());
        ProviderPricingPolicy::default()
    })
    .err()
    .expect("invalid rates passed validation");
    assert!(
        error.to_string() == "price_output must be a non-negative USD price per million tokens"
    );
    assert!(*calls.borrow() == ["b"]);
}

#[tokio::test]
async fn refresh_task_is_shared_and_stops_with_the_last_pricing_owner() {
    let pricing = Pricing::default();
    let clone = pricing.clone();
    let started = CancellationToken::new();
    let stopped = CancellationToken::new();
    let started_for_task = started.clone();
    let stop_guard = stopped.clone().drop_guard();
    let (check_tx, check_rx) = tokio::sync::oneshot::channel();
    let (alive_tx, alive_rx) = tokio::sync::oneshot::channel();
    pricing.start_refresh_task(async move {
        let _stop_guard = stop_guard;
        started_for_task.cancel();
        if check_rx.await.is_ok() {
            let _ = alive_tx.send(());
        }
        std::future::pending().await
    });
    kraai_test_support::hang_guard(started.cancelled())
        .await
        .unwrap();

    let duplicate_dropped = CancellationToken::new();
    let duplicate_guard = duplicate_dropped.clone().drop_guard();
    clone.start_refresh_task(async move {
        let _duplicate_guard = duplicate_guard;
        std::future::pending().await
    });
    assert!(
        duplicate_dropped.is_cancelled(),
        "duplicate background refresh worker was started"
    );

    drop(pricing);
    if check_tx.send(()).is_err() {
        panic!("refresh worker stopped while another pricing owner remained");
    }
    kraai_test_support::hang_guard(alive_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !stopped.is_cancelled(),
        "refresh worker stopped while another pricing owner remained"
    );
    drop(clone);
    kraai_test_support::hang_guard(stopped.cancelled())
        .await
        .unwrap();
}

#[tokio::test]
async fn configured_rates_estimate_usage_but_never_replace_a_reported_zero() {
    check_configured_rates(false).await;
    check_configured_rates(true).await;
}

async fn check_configured_rates(subscription: bool) {
    let provider = ProviderId::new("custom");
    let model = ModelId::new("model");
    let pricing = Pricing::new(
        &ProviderManagerConfig {
            providers: vec![ProviderConfig {
                id: provider.clone(),
                type_id: "custom-factory".into(),
                config: DynamicConfig::new(),
            }],
            models: vec![ModelConfig {
                options: Vec::new(),
                remove_options: Vec::new(),
                id: model.clone(),
                provider_id: provider.clone(),
                config: DynamicConfig::from([
                    ("price_input".into(), DynamicValue::from("2")),
                    ("price_output".into(), DynamicValue::from("8")),
                ]),
            }],
        },
        |_| ProviderPricingPolicy {
            subscription,
            ..ProviderPricingPolicy::default()
        },
    )
    .unwrap();
    assert_eq!(pricing.is_subscription(&provider), subscription);
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        reasoning_tokens: 10,
        ..Default::default()
    };
    let reported = TokenUsage {
        cost: Some(RequestCost {
            amount: Usd(0),
            source: "openrouter".into(),
            rates: None,
            priced_at: 1,
            upstream: None,
        }),
        ..usage.clone()
    };
    let source = futures::stream::iter(vec![
        Ok(ProviderStreamEvent::Usage(usage)),
        Ok(ProviderStreamEvent::Usage(reported)),
    ])
    .boxed();
    let mut stream = pricing.apply(&provider, &model, source).await;
    let Some(Ok(ProviderStreamEvent::Usage(estimated))) = stream.next().await else {
        panic!("missing estimated usage");
    };
    assert!(
        estimated
            .cost
            .is_some_and(|cost| cost.amount == Usd(440_000) && cost.rates.is_some())
    );
    let Some(Ok(ProviderStreamEvent::Usage(reported))) = stream.next().await else {
        panic!("missing reported usage");
    };
    assert!(
        reported
            .cost
            .is_some_and(|cost| cost.amount == Usd(0) && cost.rates.is_none())
    );
}
