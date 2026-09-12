use super::*;

#[test]
fn costs_replace_unknown_records_without_double_counting_and_restore_session_totals() {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some("session".into());
    harness.app.state.launched_at = 100;
    let mut request = kraai_types::RequestUsage {
        message_id: MessageId::new("request"),
        provider_id: kraai_types::ProviderId::new("provider"),
        model_id: kraai_types::ModelId::new("model"),
        started_at: 101,
        subscription: false,
        usage: None,
    };
    let records = |request: &kraai_types::RequestUsage| {
        std::collections::BTreeMap::from([(request.message_id.clone(), request.clone())])
    };
    harness.app.update_costs("session", records(&request));
    assert_eq!(harness.app.state.session_cost.unknown, 1);
    request.usage = Some(kraai_types::TokenUsage {
        cost: Some(kraai_types::RequestCost {
            amount: kraai_types::Usd(12_300_000),
            source: "openrouter".into(),
            rates: None,
            priced_at: 1,
            upstream: None,
        }),
        ..Default::default()
    });
    harness.app.update_costs("session", records(&request));
    harness.app.update_costs("session", records(&request));
    assert_eq!(harness.app.state.session_cost.to_string(), "$0.0123");
    assert_eq!(harness.app.state.launch_requests.len(), 1);
    assert!(
        harness
            .app
            .exit_token_usage_summary()
            .is_some_and(|summary| summary.contains("total: $0.0123"))
    );
    request.message_id = MessageId::new("historical");
    request.started_at = 1;
    harness.app.update_costs("other", records(&request));
    assert_eq!(harness.app.state.launch_requests.len(), 1);
    harness.app.state.current_session_id = Some("other".into());
    harness.app.update_costs("other", records(&request));
    assert_eq!(harness.app.state.session_cost.to_string(), "$0.0123");
}
