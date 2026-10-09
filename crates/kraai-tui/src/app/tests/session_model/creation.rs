use super::*;

fn submit_first(harness: &mut TestHarness) -> color_eyre::Result<(u64, ModelSelection)> {
    super::super::model_options::configure(harness)?;
    harness.app.startup_options.ci = true;
    let submitted = ModelSelection {
        provider_id: kraai_types::ProviderId::new("provider"),
        model_id: kraai_types::ModelId::new("model"),
        options: options("low", false, None),
    };
    harness.app.state.selected_model_options = submitted.options.clone();
    harness.app.submit_message("first message".into());
    let creation_id = harness
        .drain_requests()
        .into_iter()
        .find_map(|request| match request {
            RuntimeRequest::CreateSession { creation_id, .. } => Some(creation_id),
            _ => None,
        })
        .ok_or_else(|| color_eyre::eyre::eyre!("missing session creation"))?;
    Ok((creation_id, submitted))
}

fn created(harness: &mut TestHarness, creation_id: u64, submitted: &ModelSelection) {
    harness
        .app
        .handle_runtime_response(RuntimeResponse::CreateSession {
            creation_id,
            result: Ok("created-session".into()),
        });
    let requests = harness.drain_requests();
    assert!(requests.iter().any(|request| matches!(request,
        RuntimeRequest::SendMessage { session_id, provider_id, model_id, options, .. }
        if session_id == "created-session"
            && provider_id == submitted.provider_id.as_str()
            && model_id == submitted.model_id.as_str()
            && options == &submitted.options
    )));
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, RuntimeRequest::SetSessionModel { .. }))
    );
    snapshot(harness, "created-session", 0, session_snapshot(None));
    assert!(saves(harness).is_empty());
}

#[test]
fn edits_during_creation_survive_initial_snapshots_and_save_after_the_turn()
-> color_eyre::Result<()> {
    for change_model in [false, true] {
        let mut harness = test_harness();
        let (creation_id, submitted) = submit_first(&mut harness)?;
        if change_model {
            let mut model = harness
                .app
                .state
                .selected_model()
                .cloned()
                .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
            model.id = "other-model".into();
            harness
                .app
                .state
                .models_by_provider
                .insert("other-provider".into(), vec![model]);
            harness.app.state.selected_provider_id = Some("other-provider".into());
            harness.app.state.selected_model_id = Some("other-model".into());
        }
        harness
            .app
            .set_model_option("effort", "high")
            .map_err(color_eyre::eyre::Error::msg)?;
        let desired = ModelSelection {
            provider_id: kraai_types::ProviderId::new(if change_model {
                "other-provider"
            } else {
                "provider"
            }),
            model_id: kraai_types::ModelId::new(if change_model { "other-model" } else { "model" }),
            options: options("high", false, None),
        };
        created(&mut harness, creation_id, &submitted);
        harness
            .app
            .handle_runtime_response(RuntimeResponse::SendMessage {
                session_id: "created-session".into(),
                result: Ok(kraai_runtime::SubmitMessageOutcome::Started {
                    message_id: "reply".into(),
                }),
            });
        let mut active = session_snapshot(None);
        active.activity = SessionActivity::Streaming;
        active.session.is_running = true;
        active.session.selected_model = Some(submitted.clone());
        snapshot(&mut harness, "created-session", 0, active);
        assert_eq!(harness.app.state.selected_model_options, desired.options);
        assert_eq!(
            harness.app.state.selected_provider_id.as_deref(),
            Some(desired.provider_id.as_str())
        );
        assert_eq!(
            harness.app.state.selected_model_id.as_deref(),
            Some(desired.model_id.as_str())
        );
        assert!(saves(&harness).is_empty());
        let mut idle = session_snapshot(None);
        idle.session.selected_model = Some(submitted);
        snapshot(&mut harness, "created-session", 0, idle);
        assert!(
            matches!(saves(&harness).as_slice(), [(session, _, selection)]
            if session == "created-session" && selection == &desired)
        );
    }
    Ok(())
}

#[test]
fn creation_edits_save_after_initial_admission_fails() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    let (creation_id, submitted) = submit_first(&mut harness)?;
    harness
        .app
        .set_model_option("effort", "high")
        .map_err(color_eyre::eyre::Error::msg)?;
    created(&mut harness, creation_id, &submitted);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "created-session".into(),
            result: Err(kraai_runtime::RuntimeError::unavailable("admission failed")),
        });
    snapshot(&mut harness, "created-session", 0, session_snapshot(None));
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session == "created-session" && selection.options == options("high", false, None))
    );
    assert_eq!(harness.app.state.input, "first message");
    Ok(())
}

#[test]
fn unchanged_creation_selection_does_not_schedule_an_extra_save() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    let (creation_id, submitted) = submit_first(&mut harness)?;
    created(&mut harness, creation_id, &submitted);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "created-session".into(),
            result: Ok(kraai_runtime::SubmitMessageOutcome::Started {
                message_id: "reply".into(),
            }),
        });
    let mut idle = session_snapshot(None);
    idle.session.selected_model = Some(submitted);
    snapshot(&mut harness, "created-session", 0, idle);
    assert!(saves(&harness).is_empty());
    Ok(())
}
