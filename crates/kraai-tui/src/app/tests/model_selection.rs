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
fn default_selection_skips_empty_providers_and_is_deterministic() {
    for reverse in [false, true] {
        let mut harness = test_harness();
        harness.app.startup_options.ci = true;
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
        for selected_provider in [None, Some("empty"), Some("unavailable")] {
            harness.app.state.selected_provider_id = selected_provider.map(String::from);
            harness.app.state.selected_model_id = None;
            harness.app.ensure_selected_model();
            assert_eq!(
                harness.app.state.selected_provider_id.as_deref(),
                Some("alpha")
            );
            assert_eq!(
                harness.app.state.selected_model_id.as_deref(),
                Some("first")
            );
        }
    }
}

#[test]
fn fallback_keeps_matching_model_and_prefers_existing_provider() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
    harness.app.state.models_by_provider = HashMap::from([
        ("empty".into(), Vec::new()),
        ("alpha".into(), vec![model("first"), model("shared")]),
        ("zeta".into(), vec![model("shared")]),
    ]);
    for (provider, expected) in [("missing", "alpha"), ("empty", "alpha"), ("zeta", "zeta")] {
        harness.app.state.selected_provider_id = Some(provider.into());
        harness.app.state.selected_model_id = Some("shared".into());
        harness.app.ensure_selected_model();
        assert_eq!(
            harness.app.state.selected_provider_id.as_deref(),
            Some(expected)
        );
        assert_eq!(
            harness.app.state.selected_model_id.as_deref(),
            Some("shared")
        );
    }
}

#[test]
fn empty_discovery_preserves_selection_for_recovery() {
    let mut harness = test_harness();
    harness.app.startup_options.ci = true;
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
