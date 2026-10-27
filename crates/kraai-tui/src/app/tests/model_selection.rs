use super::*;

fn model(id: &str) -> kraai_runtime::Model {
    kraai_runtime::Model {
        id: id.into(),
        name: id.into(),
        max_context: None,
        options: Vec::new(),
    }
}

#[test]
fn default_selection_is_deterministic_and_stays_with_the_selected_provider() {
    for reverse in [false, true] {
        let mut harness = test_harness();
        let mut providers = [
            ("empty", Vec::new()),
            ("alpha", vec![model("first"), model("second")]),
            ("zeta", vec![model("other")]),
        ];
        if reverse {
            providers.reverse();
        }
        harness.app.state.models_by_provider = providers
            .into_iter()
            .map(|(provider, models)| (provider.into(), models))
            .collect();
        for (selected_provider, expected) in [
            (None, Some(("alpha", "first"))),
            (Some("zeta"), Some(("zeta", "other"))),
            (Some("empty"), None),
            (Some("unavailable"), None),
        ] {
            harness.app.state.selected_provider_id = selected_provider.map(String::from);
            harness.app.state.selected_model_id = None;
            harness.app.ensure_selected_model();
            assert_eq!(
                harness.app.state.selected_provider_id.as_deref(),
                expected.map(|(provider, _)| provider).or(selected_provider)
            );
            assert_eq!(
                harness.app.state.selected_model_id.as_deref(),
                expected.map(|(_, model)| model)
            );
        }
    }
}

#[test]
fn a_selected_model_never_moves_to_another_provider() {
    let mut harness = test_harness();
    harness.app.state.models_by_provider = HashMap::from([
        ("empty".into(), Vec::new()),
        ("alpha".into(), vec![model("first"), model("shared")]),
        ("zeta".into(), vec![model("shared")]),
    ]);
    for provider in ["missing", "empty", "zeta"] {
        harness.app.state.selected_provider_id = Some(provider.into());
        harness.app.state.selected_model_id = Some("shared".into());
        harness.app.ensure_selected_model();
        assert_eq!(
            harness.app.state.selected_provider_id.as_deref(),
            Some(provider)
        );
        assert_eq!(
            harness.app.state.selected_model_id.as_deref(),
            Some("shared")
        );
    }
}

#[test]
fn loading_models_never_saves_over_the_session_model() {
    for (selected, expected) in [(Some("added-later"), "added-later"), (None, "first")] {
        let mut harness = test_harness();
        harness.app.state.current_session_id = Some(kraai_types::SessionId::new("session"));
        harness.app.state.selected_provider_id = Some("provider".into());
        harness.app.state.selected_model_id = selected.map(String::from);
        harness.drain_requests();
        harness
            .app
            .handle_runtime_response(RuntimeResponse::Models(Ok(HashMap::from([(
                "provider".into(),
                vec![model("first")],
            )]))));
        assert_eq!(
            harness.app.state.selected_model_id.as_deref(),
            Some(expected)
        );
        assert!(
            !harness
                .drain_requests()
                .iter()
                .any(|request| matches!(request, RuntimeRequest::SetSessionModel { .. }))
        );
    }
}

#[test]
fn empty_discovery_preserves_selection_for_recovery() {
    let mut harness = test_harness();
    harness.app.state.selected_provider_id = Some("provider".into());
    harness.app.state.selected_model_id = Some("model".into());
    harness.app.state.models_by_provider = HashMap::from([("provider".into(), Vec::new())]);
    harness.app.ensure_selected_model();
    assert_eq!(
        harness.app.state.selected_provider_id.as_deref(),
        Some("provider")
    );
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("model")
    );
}
