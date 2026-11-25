use super::*;

fn switch_to_other_session(harness: &mut TestHarness) {
    harness
        .app
        .reset_chat_session(Some(SessionId::new("other")));
    harness.app.state.models.selected_options.clear();
}

fn remove_effort(harness: &mut TestHarness) {
    let mut models = harness.app.state.models.by_provider.clone();
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
fn metadata_refresh_reconciles_deferred_saves_before_the_turn_finishes() {
    let mut harness = test_harness();
    configure(&mut harness);
    let mut active = session_snapshot(None);
    active.session.is_running = true;
    active.activity = SessionActivity::Streaming;
    snapshot(&mut harness, &SessionId::new("session"), 0, active);
    set_option(&mut harness, "effort", "high").unwrap();
    assert!(saves(&harness).is_empty());
    remove_effort(&mut harness);
    assert!(harness.app.state.models.selected_options.is_empty());
    assert!(saves(&harness).is_empty());
    snapshot(
        &mut harness,
        &SessionId::new("session"),
        0,
        session_snapshot(None),
    );
    assert!(matches!(saves(&harness).as_slice(), [(_, _, selection)]
        if selection.options.is_empty()));
}

#[test]
fn unchanged_catalog_metadata_does_not_write_session_selection() {
    let mut harness = test_harness();
    configure(&mut harness);
    let models = harness.app.state.models.by_provider.clone();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Models(Ok(models)));
    assert!(saves(&harness).is_empty());
}

#[test]
fn metadata_refresh_reconciles_background_saves_before_the_turn_finishes() {
    let mut harness = test_harness();
    configure(&mut harness);
    let mut active = session_snapshot(None);
    active.session.is_running = true;
    active.activity = SessionActivity::Streaming;
    snapshot(&mut harness, &SessionId::new("session"), 0, active);
    harness.app.state.models.selected_options = options("high", false, None);
    harness.app.save_model_selection();
    assert!(saves(&harness).is_empty());
    switch_to_other_session(&mut harness);
    remove_effort(&mut harness);
    assert!(saves(&harness).is_empty());
    snapshot(
        &mut harness,
        &SessionId::new("session"),
        0,
        session_snapshot(None),
    );
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session.as_str() == "session"
            && selection.provider_id.as_str() == "provider"
            && selection.model_id.as_str() == "model"
            && selection.options == remaining_options())
    );
    assert!(harness.app.state.models.selected_options.is_empty());
}

#[test]
fn stale_background_save_results_preserve_reconciled_pending_options() {
    for result in [
        Ok(()),
        Err(kraai_runtime::RuntimeError::invalid_argument(
            "Unknown option effort",
        )),
    ] {
        let mut harness = test_harness();
        configure(&mut harness);
        harness.app.state.models.selected_options = options("high", false, None);
        harness.app.save_model_selection();
        let (_, initial_id, initial) = saves(&harness)
            .into_iter()
            .next()
            .expect("missing initial save");
        switch_to_other_session(&mut harness);
        remove_effort(&mut harness);
        assert!(saves(&harness).is_empty());
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SetSessionModel {
                session_id: SessionId::new("session"),
                save_id: initial_id,
                result,
            });
        let (_, corrected_id, corrected) = saves(&harness)
            .into_iter()
            .next()
            .expect("missing corrected save");
        assert!(corrected_id > initial_id);
        assert_eq!(corrected.options, remaining_options());
        assert_eq!(corrected.provider_id, initial.provider_id);
        assert_eq!(corrected.model_id, initial.model_id);
        acknowledge(&mut harness, &SessionId::new("session"), initial_id);
        let mut stale = session_snapshot(None);
        stale.session.selected_model = Some(initial);
        snapshot(&mut harness, &SessionId::new("session"), initial_id, stale);
        assert!(saves(&harness).is_empty());
        acknowledge(&mut harness, &SessionId::new("session"), corrected_id);
        assert!(saves(&harness).is_empty());
        assert!(harness.app.state.models.selected_options.is_empty());
        assert_eq!(
            harness.app.state.current_session_id.as_ref(),
            Some(&SessionId::new("other"))
        );
    }
}

#[test]
fn metadata_refresh_retries_background_saves_rejected_before_refresh() {
    let mut harness = test_harness();
    configure(&mut harness);
    harness.app.state.models.selected_options = options("high", false, None);
    harness.app.save_model_selection();
    let (_, save_id, _) = saves(&harness)
        .into_iter()
        .next()
        .expect("missing initial save");
    switch_to_other_session(&mut harness);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionModel {
            session_id: SessionId::new("session"),
            save_id,
            result: Err(kraai_runtime::RuntimeError::invalid_argument(
                "Unknown option effort",
            )),
        });
    snapshot(
        &mut harness,
        &SessionId::new("session"),
        0,
        session_snapshot(None),
    );
    assert!(saves(&harness).is_empty());
    remove_effort(&mut harness);
    assert!(
        matches!(saves(&harness).as_slice(), [(session, corrected_id, selection)]
        if session.as_str() == "session" && *corrected_id > save_id
            && selection.options == remaining_options())
    );
    assert!(harness.app.state.models.selected_options.is_empty());
}
