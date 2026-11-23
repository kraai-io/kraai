use super::*;

fn usage(input: u64, cache_read: u64, output: u64) -> UsageMetrics {
    UsageMetrics {
        total_tokens: input + cache_read + output,
        input_tokens: input,
        cache_read_tokens: cache_read,
        output_tokens: output,
        ..UsageMetrics::default()
    }
}

#[test]
fn saved_prices_price_every_request_by_its_model() {
    let temporary =
        std::env::temp_dir().join(format!("kraai-eval-prices-{}", ulid::Ulid::generate()));
    let root = EvalRoot::open(&temporary).unwrap();
    assert!(root.model_price("model").unwrap().is_none());
    let price = ModelPrice {
        input: 2.0,
        output: 10.0,
        cache_read: Some(0.5),
        cache_write: None,
    };
    root.save_model_price("model", price).unwrap();
    root.save_model_price(
        "other",
        ModelPrice {
            input: 1.0,
            output: 1.0,
            cache_read: None,
            cache_write: None,
        },
    )
    .unwrap();
    assert!(root.model_price("model").unwrap() == Some(price));
    assert!(
        root.save_model_price(
            "bad",
            ModelPrice {
                input: -1.0,
                ..price
            }
        )
        .is_err()
    );
    let prices = Prices::load(root.path()).unwrap();
    assert!(
        prices.usage_cost("model", &usage(1_000_000, 2_000_000, 100_000))
            == Some(Usd(4_000_000_000))
    );
    assert!(prices.usage_cost("other", &usage(0, 1, 0)).is_none());
    assert!(prices.usage_cost("missing", &usage(1, 0, 0)).is_none());
    let request = |model: &str| {
        serde_json::json!({
            "event_index": 0, "model": model,
            "usage": usage(1_000_000, 0, 0),
        })
    };
    let accounting =
        |requests: Vec<serde_json::Value>, unrecorded: u64| -> Result<RequestAccounting> {
            Ok(RequestAccounting {
                model_requests: requests.len() as u64,
                unrecorded_requests: unrecorded,
                requests: requests
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<_, _>>()?,
                ..RequestAccounting::default()
            })
        };
    assert!(
        prices.accounting_cost(&accounting(vec![request("model"), request("other")], 0).unwrap())
            == Some(Usd(3_000_000_000))
    );
    assert!(
        prices
            .accounting_cost(&accounting(vec![request("model"), request("missing")], 0).unwrap())
            .is_none()
    );
    assert!(
        prices
            .accounting_cost(&accounting(vec![request("model")], 1).unwrap())
            .is_none()
    );
    assert!(
        prices
            .accounting_cost(&accounting(vec![], 0).unwrap())
            .is_none()
    );
    fs::remove_dir_all(temporary).unwrap();
}
