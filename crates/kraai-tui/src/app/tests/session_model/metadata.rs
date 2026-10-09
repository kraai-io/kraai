use super::*;

fn configure_refresh(harness: &mut TestHarness) -> color_eyre::Result<()> {
    configure(harness)?;
    harness.app.startup_options.provider_id = Some("provider".into());
    harness.app.startup_options.model_id = Some("model".into());
    Ok(())
}

fn switch_to_other_session(harness: &mut TestHarness) {
    harness
        .app
        .reset_chat_session(Some("other".into()), "other session");
    harness.app.state.selected_model_options.clear();
}

fn remove_effort(harness: &mut TestHarness) {
    let mut models = harness.app.state.models_by_provider.clone();
    for model in models.values_mut().flatten() {
        model.options.retain(|option| option.id != "effort");
    }
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
}

fn remaining_options() -> ModelOptionValues {
    ModelOptionValues::from([("thinking".into(), ModelOptionValue::Boolean(false))])
}

#[test]
fn metadata_refresh_reconciles_deferred_saves_before_the_turn_finishes() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    let mut active = session_snapshot(None);
    active.session.is_running = true;
    active.activity = SessionActivity::Streaming;
    snapshot(&mut harness, "session", 0, active);
    harness
        .app
        .set_model_option("effort", "high")
        .map_err(color_eyre::eyre::Error::msg)?;
    assert!(saves(&harness).is_empty());
    remove_effort(&mut harness);
    assert!(harness.app.state.selected_model_options.is_empty());
    assert!(saves(&harness).is_empty());
    snapshot(&mut harness, "session", 0, session_snapshot(None));
    assert!(matches!(saves(&harness).as_slice(), [(_, _, selection)]
        if selection.options.is_empty()));
    Ok(())
}

#[test]
fn unchanged_catalog_metadata_does_not_write_session_selection() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    let models = harness.app.state.models_by_provider.clone();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert!(saves(&harness).is_empty());
    Ok(())
}

#[test]
fn metadata_refresh_reconciles_background_saves_before_the_turn_finishes() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    let mut active = session_snapshot(None);
    active.session.is_running = true;
    active.activity = SessionActivity::Streaming;
    snapshot(&mut harness, "session", 0, active);
    harness.app.state.selected_model_options = options("high", false, None);
    harness.app.save_model_selection();
    assert!(saves(&harness).is_empty());
    switch_to_other_session(&mut harness);
    remove_effort(&mut harness);
    assert!(saves(&harness).is_empty());
    snapshot(&mut harness, "session", 0, session_snapshot(None));
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session == "session"
            && selection.provider_id.as_str() == "provider"
            && selection.model_id.as_str() == "model"
            && selection.options == remaining_options())
    );
    assert!(harness.app.state.selected_model_options.is_empty());
    Ok(())
}

#[test]
fn stale_background_save_results_preserve_reconciled_pending_options() -> color_eyre::Result<()> {
    for result in [
        Ok(()),
        Err(kraai_runtime::RuntimeError::invalid_argument(
            "Unknown option effort",
        )),
    ] {
        let mut harness = test_harness();
        configure_refresh(&mut harness)?;
        harness.app.state.selected_model_options = options("high", false, None);
        harness.app.save_model_selection();
        let (_, initial_id, initial) = saves(&harness)
            .into_iter()
            .next()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing initial save"))?;
        switch_to_other_session(&mut harness);
        remove_effort(&mut harness);
        assert!(saves(&harness).is_empty());
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SetSessionModel {
                session_id: "session".into(),
                save_id: initial_id,
                result,
            });
        let (_, corrected_id, corrected) = saves(&harness)
            .into_iter()
            .next()
            .ok_or_else(|| color_eyre::eyre::eyre!("missing corrected save"))?;
        assert!(corrected_id > initial_id);
        assert_eq!(corrected.options, remaining_options());
        assert_eq!(corrected.provider_id, initial.provider_id);
        assert_eq!(corrected.model_id, initial.model_id);
        acknowledge(&mut harness, "session", initial_id);
        let mut stale = session_snapshot(None);
        stale.session.selected_model = Some(initial);
        snapshot(&mut harness, "session", initial_id, stale);
        assert!(saves(&harness).is_empty());
        acknowledge(&mut harness, "session", corrected_id);
        assert!(saves(&harness).is_empty());
        assert!(harness.app.state.selected_model_options.is_empty());
        assert_eq!(
            harness.app.state.current_session_id.as_deref(),
            Some("other")
        );
    }
    Ok(())
}

#[test]
fn metadata_refresh_retries_background_saves_rejected_before_refresh() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure_refresh(&mut harness)?;
    harness.app.state.selected_model_options = options("high", false, None);
    harness.app.save_model_selection();
    let (_, save_id, _) = saves(&harness)
        .into_iter()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing initial save"))?;
    switch_to_other_session(&mut harness);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionModel {
            session_id: "session".into(),
            save_id,
            result: Err(kraai_runtime::RuntimeError::invalid_argument(
                "Unknown option effort",
            )),
        });
    snapshot(&mut harness, "session", 0, session_snapshot(None));
    assert!(saves(&harness).is_empty());
    remove_effort(&mut harness);
    assert!(
        matches!(saves(&harness).as_slice(), [(session, corrected_id, selection)]
        if session == "session" && *corrected_id > save_id
            && selection.options == remaining_options())
    );
    assert!(harness.app.state.selected_model_options.is_empty());
    Ok(())
}
