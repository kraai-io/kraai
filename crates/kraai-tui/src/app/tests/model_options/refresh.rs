use super::*;
use kraai_types::SessionId;

fn refreshed_models(
    harness: &TestHarness,
    update: impl FnOnce(&mut kraai_runtime::Model),
) -> color_eyre::Result<HashMap<String, Vec<kraai_runtime::Model>>> {
    let mut models = harness.app.state.models_by_provider.clone();
    let model = models
        .get_mut("provider")
        .and_then(|models| models.first_mut())
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    update(model);
    Ok(models)
}

#[test]
fn metadata_reordering_keeps_the_highlight_on_the_same_setting() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "effort")?;
    let models = refreshed_models(&harness, |model| model.options.reverse())?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert_eq!(harness.app.state.sheet_index, 2);
    key(&mut harness, KeyCode::Left);
    assert_eq!(
        harness.app.state.selected_model_options,
        ModelOptionValues::from([
            ("effort".into(), ModelOptionValue::Choice("low".into())),
            ("thinking".into(), ModelOptionValue::Boolean(true)),
            ("budget".into(), ModelOptionValue::Integer(50)),
        ])
    );
    assert!(harness.app.state.last_error.is_none());
    Ok(())
}

#[test]
fn changed_choice_order_steps_from_the_current_value() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "effort")?;
    let models = refreshed_models(&harness, |model| {
        for option in &mut model.options {
            if let ModelOptionKind::Choice { choices } = &mut option.kind {
                choices.reverse();
            }
        }
    })?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    key(&mut harness, KeyCode::Right);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    Ok(())
}

#[test]
fn disappearing_setting_keeps_the_remaining_settings_reachable() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "budget")?;
    type_text(&mut harness, "7");
    let models = refreshed_models(&harness, |model| {
        model.options.retain(|option| option.id != "budget");
    })?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert_eq!(harness.app.state.sheet_index, 1);
    assert_eq!(value(&harness, "budget"), None);
    key(&mut harness, KeyCode::Enter);
    assert_eq!(
        value(&harness, "thinking"),
        Some(&ModelOptionValue::Boolean(true))
    );
    assert!(harness.app.state.sheet_notice.is_some());
    key(&mut harness, KeyCode::Backspace);
    key(&mut harness, KeyCode::Up);
    assert_eq!(harness.app.state.sheet_index, 0);
    key(&mut harness, KeyCode::Left);
    assert_eq!(
        value(&harness, "effort"),
        Some(&ModelOptionValue::Choice("low".into()))
    );
    Ok(())
}

#[test]
fn losing_every_setting_leaves_an_empty_sheet_that_enter_closes() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_optional(&mut harness)?;
    open_settings(&mut harness, "budget")?;
    let models = refreshed_models(&harness, |model| model.options.clear())?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert!(screen(&harness.app.state, 80, 20).contains("Model has no settings"));
    for code in [KeyCode::Left, KeyCode::Delete, KeyCode::Down] {
        key(&mut harness, code);
        assert_eq!(harness.app.state.mode, UiMode::ModelSettings);
    }
    key(&mut harness, KeyCode::Enter);
    assert_eq!(harness.app.state.mode, UiMode::Chat);
    assert!(harness.app.state.last_error.is_none());
    Ok(())
}

#[test]
fn a_session_switching_models_keeps_the_sheet_on_the_model_it_names() -> color_eyre::Result<()> {
    for (provider, model_id) in [("provider", "other-model"), ("other-provider", "model")] {
        let mut harness = test_harness();
        configure_optional(&mut harness)?;
        open_settings(&mut harness, "effort")?;
        let mut model = harness
            .app
            .state
            .selected_model()
            .cloned()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
        model.id = model_id.into();
        harness
            .app
            .state
            .models_by_provider
            .entry(provider.into())
            .or_default()
            .push(model);
        harness.app.state.current_session_id = Some(SessionId::new("session"));
        let mut snapshot = session_snapshot(None);
        let values = harness.app.state.selected_model_options.clone();
        snapshot.session.selected_model = Some(kraai_types::ModelSelection {
            provider_id: kraai_types::ProviderId::new(provider),
            model_id: kraai_types::ModelId::new(model_id),
            options: values.clone(),
        });
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SessionSnapshot {
                session_id: SessionId::new("session"),
                model_save_id: 0,
                result: Box::new(Ok(snapshot)),
            });
        assert_eq!(
            harness.app.state.selected_model_id.as_deref(),
            Some(model_id)
        );
        harness.drain_requests();
        key(&mut harness, KeyCode::Left);
        assert_eq!(harness.app.state.selected_model_options, values);
        assert!(
            !harness
                .drain_requests()
                .into_iter()
                .any(|request| matches!(request, RuntimeRequest::SetSessionModel { .. }))
        );
        assert_eq!(
            harness
                .app
                .state
                .remembered_model_options
                .get("provider")
                .and_then(|models| models.get("model"))
                .and_then(|values| values.get("effort")),
            Some(&ModelOptionValue::Choice("low".into()))
        );
    }
    Ok(())
}
