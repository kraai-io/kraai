use super::*;

fn configure_refresh(harness: &mut TestHarness) -> color_eyre::Result<()> {
    configure_optional(harness)?;
    harness.app.startup_options.provider_id = Some("provider".into());
    harness.app.startup_options.model_id = Some("model".into());
    Ok(())
}

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
fn metadata_reordering_preserves_the_option_being_edited() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    edit(&mut harness, "effort")?;
    key(&mut harness, KeyCode::Up);
    let models = refreshed_models(&harness, |model| model.options.reverse())?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert!(harness.app.state.option_editor.is_some());
    assert_eq!(harness.app.state.option_menu_index, 2);
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editor.is_none());
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
fn changed_choice_order_cancels_edit_instead_of_selecting_another_value() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    edit(&mut harness, "effort")?;
    let values = harness.app.state.selected_model_options.clone();
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
    assert!(harness.app.state.option_editor.is_none());
    assert_eq!(harness.app.state.selected_model_options, values);
    assert_eq!(
        harness.app.state.option_editor_error.as_deref(),
        Some("Model options changed. Choose an option again.")
    );
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editor.is_some());
    assert_eq!(harness.app.state.selected_model_options, values);
    assert!(harness.app.state.option_editor_error.is_none());
    Ok(())
}

#[test]
fn disappearing_option_cancels_edit_and_keeps_the_remaining_options_reachable()
-> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    edit(&mut harness, "budget")?;
    let models = refreshed_models(&harness, |model| {
        model.options.retain(|option| option.id != "budget");
    })?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert!(harness.app.state.option_editor.is_none());
    assert!(harness.app.state.option_editor_input.is_empty());
    assert_eq!(harness.app.state.option_menu_index, 1);
    key(&mut harness, KeyCode::Up);
    assert_eq!(harness.app.state.option_menu_index, 0);
    key(&mut harness, KeyCode::Enter);
    assert!(harness.app.state.option_editor.is_some());
    assert_eq!(
        harness.app.state.selected_model_options.get("effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    Ok(())
}

#[test]
fn metadata_changes_after_closing_editor_do_not_replace_unrelated_errors() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    edit(&mut harness, "budget")?;
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    harness.app.set_error("Unrelated runtime error".into());
    let models = refreshed_models(&harness, |model| model.options.clear())?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert!(harness.app.state.option_editor.is_none());
    assert!(harness.app.state.option_editor_error.is_none());
    assert_eq!(
        harness.app.state.last_error.as_deref(),
        Some("Unrelated runtime error")
    );
    Ok(())
}

#[test]
fn changed_model_selection_cancels_editor_even_with_identical_definitions() -> color_eyre::Result<()>
{
    for (provider, model_id) in [("provider", "other-model"), ("other-provider", "model")] {
        let mut harness = test_harness();
        configure_refresh(&mut harness)?;
        edit(&mut harness, "effort")?;
        key(&mut harness, KeyCode::Up);
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
        harness.app.state.current_session_id = Some("session".into());
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
                session_id: "session".into(),
                model_save_id: 0,
                result: Box::new(Ok(snapshot)),
            });
        assert!(harness.app.state.option_editor.is_none());
        key(&mut harness, KeyCode::Enter);
        assert!(harness.app.state.option_editor.is_some());
        assert_eq!(harness.app.state.selected_model_options, values);
    }
    Ok(())
}
