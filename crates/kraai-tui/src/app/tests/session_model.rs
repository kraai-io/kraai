#![expect(
    clippy::panic_in_result_fn,
    reason = "session model tests assert after fallible fixture parsing"
)]

use kraai_types::{ModelOptionValue, ModelOptionValues, ModelSelection};

use super::*;

mod creation;
mod metadata;

fn configure(harness: &mut TestHarness) -> color_eyre::Result<()> {
    super::model_options::configure(harness)?;
    harness.app.startup_options.ci = true;
    harness
        .app
        .reset_chat_session(Some("session".into()), "loaded");
    snapshot(harness, "session", 0, session_snapshot(None));
    harness.drain_requests();
    Ok(())
}

fn snapshot(
    harness: &mut TestHarness,
    session_id: &str,
    model_save_id: u64,
    mut snapshot: SessionSnapshot,
) {
    snapshot.session.id = session_id.into();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: session_id.into(),
            model_save_id,
            result: Box::new(Ok(snapshot)),
        });
}

fn saves(harness: &TestHarness) -> Vec<(String, u64, ModelSelection)> {
    harness
        .drain_requests()
        .into_iter()
        .filter_map(|request| match request {
            RuntimeRequest::SetSessionModel {
                session_id,
                save_id,
                selection,
            } => Some((session_id, save_id, selection)),
            _ => None,
        })
        .collect()
}

fn acknowledge(harness: &mut TestHarness, session_id: &str, save_id: u64) {
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionModel {
            session_id: session_id.into(),
            save_id,
            result: Ok(()),
        });
}

fn options(effort: &str, thinking: bool, budget: Option<i64>) -> ModelOptionValues {
    let mut values = ModelOptionValues::from([
        ("effort".into(), ModelOptionValue::Choice(effort.into())),
        ("thinking".into(), ModelOptionValue::Boolean(thinking)),
    ]);
    if let Some(budget) = budget {
        values.insert("budget".into(), ModelOptionValue::Integer(budget));
    }
    values
}

#[test]
fn option_picker_saves_partial_choices_before_any_message() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.open_model_options();
    for code in [
        super::super::KeyCode::Enter,
        super::super::KeyCode::Down,
        super::super::KeyCode::Enter,
    ] {
        harness
            .app
            .handle_model_options_key_event(super::super::KeyEvent::new(
                code,
                super::super::KeyModifiers::NONE,
            ));
    }
    let requests = harness.drain_requests();
    assert!(
        matches!(requests.as_slice(), [RuntimeRequest::SetSessionModel { session_id, selection, .. }]
        if session_id == "session" && selection.model_id.as_str() == "model" && selection.provider_id.as_str() == "provider"
            && selection.options == ModelOptionValues::from([("effort".into(), ModelOptionValue::Choice("high".into()))]))
    );
    assert!(harness.app.state.pending_messages.is_empty());
    Ok(())
}

#[test]
fn clearing_an_optional_parent_saves_the_map_without_its_dependents() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    super::model_options::configure_optional(&mut harness)?;
    harness
        .app
        .handle_option_command(vec!["thinking", "--clear"]);
    let expected =
        ModelOptionValues::from([("effort".into(), ModelOptionValue::Choice("high".into()))]);
    assert_eq!(harness.app.state.selected_model_options, expected);
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session == "session" && selection.options == expected)
    );
    Ok(())
}

#[test]
fn model_picker_saves_the_complete_typed_selection() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    let models = harness
        .app
        .state
        .models_by_provider
        .get_mut("provider")
        .ok_or_else(|| color_eyre::eyre::eyre!("missing provider"))?;
    let mut second = models
        .first()
        .cloned()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing model"))?;
    second.id = "second".into();
    models.push(second);
    let values = options("high", true, Some(50));
    harness.app.state.selected_model_options = values.clone();
    harness.app.state.model_menu_index = harness
        .app
        .state
        .filtered_models()
        .iter()
        .position(|(_, model)| model.id == "second")
        .ok_or_else(|| color_eyre::eyre::eyre!("missing second model"))?;
    harness
        .app
        .handle_model_menu_key_event(super::super::KeyEvent::new(
            super::super::KeyCode::Enter,
            super::super::KeyModifiers::NONE,
        ));
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session == "session" && selection.model_id.as_str() == "second" && selection.options == values)
    );
    Ok(())
}

#[test]
fn rapid_edits_coalesce_and_stale_responses_preserve_the_latest_selection() -> color_eyre::Result<()>
{
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness
        .app
        .set_model_option("effort", "high")
        .map_err(color_eyre::eyre::Error::msg)?;
    let (_, first_id, first) = saves(&harness)
        .into_iter()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing first save"))?;
    for (id, value) in [("effort", "low"), ("thinking", "true"), ("budget", "50")] {
        harness
            .app
            .set_model_option(id, value)
            .map_err(color_eyre::eyre::Error::msg)?;
    }
    assert!(saves(&harness).is_empty());
    let desired = options("low", true, Some(50));
    let mut stale = session_snapshot(None);
    stale.session.selected_model = Some(first.clone());
    snapshot(&mut harness, "session", 0, stale.clone());
    assert_eq!(harness.app.state.selected_model_options, desired);
    acknowledge(&mut harness, "session", first_id);
    let (_, second_id, second) = saves(&harness)
        .into_iter()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing coalesced save"))?;
    assert!(second_id > first_id);
    assert_eq!(second.options, desired);
    acknowledge(&mut harness, "session", first_id);
    assert!(harness.drain_requests().is_empty());
    acknowledge(&mut harness, "session", second_id);
    harness.drain_requests();
    stale.session.tip_id = Some("new-history".into());
    snapshot(&mut harness, "session", first_id, stale);
    assert_eq!(harness.app.state.selected_model_options, desired);
    assert_eq!(
        harness.app.state.current_tip_id.as_deref(),
        Some("new-history")
    );
    let mut confirmed = session_snapshot(None);
    confirmed.session.selected_model = Some(second);
    snapshot(&mut harness, "session", second_id, confirmed);
    assert_eq!(harness.app.state.selected_model_options, desired);
    Ok(())
}

#[test]
fn active_and_queued_turns_defer_saves_and_retain_submitted_options() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    let submitted = options("high", false, None);
    harness.app.state.selected_model_options = submitted.clone();
    let mut active = session_snapshot(None);
    active.activity = SessionActivity::Streaming;
    active.session.is_running = true;
    active.session.profile_locked = true;
    snapshot(&mut harness, "session", 0, active);
    harness.app.submit_message("queued".into());
    harness
        .app
        .set_model_option("effort", "low")
        .map_err(color_eyre::eyre::Error::msg)?;
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::SendMessage { options, .. } if options == &submitted)));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "session".into(),
            result: Ok(kraai_runtime::SubmitMessageOutcome::Queued { position: 1 }),
        });
    let mut queued = session_snapshot(None);
    queued.queued_messages = 1;
    snapshot(&mut harness, "session", 0, queued);
    assert!(saves(&harness).is_empty());
    snapshot(&mut harness, "session", 0, session_snapshot(None));
    assert!(
        matches!(saves(&harness).as_slice(), [(_, _, selection)] if selection.options == options("low", false, None))
    );
    Ok(())
}

#[test]
fn session_switches_keep_deferred_saves_and_ignore_background_acknowledgements()
-> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    let mut active = session_snapshot(None);
    active.session.is_running = true;
    active.activity = SessionActivity::Streaming;
    snapshot(&mut harness, "session", 0, active);
    harness
        .app
        .set_model_option("effort", "high")
        .map_err(color_eyre::eyre::Error::msg)?;
    assert!(saves(&harness).is_empty());
    harness
        .app
        .reset_chat_session(Some("other".into()), "other session");
    harness.app.state.selected_model_id = Some("other-model".into());
    harness.app.state.selected_model_options.clear();
    let idle = session_snapshot(None);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Sessions(Ok(vec![idle.session.clone()])));
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::GetSessionSnapshot { session_id } if session_id == "session")));
    snapshot(&mut harness, "session", 0, idle);
    let (_, save_id, selection) = saves(&harness)
        .into_iter()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing background save"))?;
    acknowledge(&mut harness, "session", save_id);
    assert_eq!(
        harness.app.state.selected_model_id.as_deref(),
        Some("other-model")
    );
    assert!(harness.app.state.selected_model_options.is_empty());
    harness
        .app
        .reset_chat_session(Some("session".into()), "loaded");
    let mut restored = session_snapshot(None);
    restored.session.selected_model = Some(selection);
    snapshot(&mut harness, "session", save_id, restored);
    assert_eq!(
        harness.app.state.selected_model_options.get("effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
    Ok(())
}

#[test]
fn admission_failure_refreshes_idle_state_and_flushes_deferred_edits() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness.app.state.selected_model_options = options("high", false, None);
    harness.app.submit_message("message".into());
    harness
        .app
        .set_model_option("effort", "low")
        .map_err(color_eyre::eyre::Error::msg)?;
    assert!(saves(&harness).is_empty());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: "session".into(),
            result: Err(kraai_runtime::RuntimeError::unavailable("admission failed")),
        });
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::GetSessionSnapshot { session_id } if session_id == "session")));
    snapshot(&mut harness, "session", 0, session_snapshot(None));
    assert!(
        matches!(saves(&harness).as_slice(), [(_, _, selection)] if selection.options == options("low", false, None))
    );
    Ok(())
}

#[test]
fn transient_save_errors_retry_during_session_reconciliation() -> color_eyre::Result<()> {
    let mut harness = test_harness();
    configure(&mut harness)?;
    harness
        .app
        .set_model_option("effort", "high")
        .map_err(color_eyre::eyre::Error::msg)?;
    let (_, first_id, desired) = saves(&harness)
        .into_iter()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing first save"))?;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionModel {
            session_id: "session".into(),
            save_id: first_id,
            result: Err(kraai_runtime::RuntimeError::unavailable(
                "temporarily unavailable",
            )),
        });
    assert!(harness.drain_requests().is_empty());
    let idle = session_snapshot(None);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Sessions(Ok(vec![idle.session.clone()])));
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::GetSessionSnapshot { session_id } if session_id == "session")));
    snapshot(&mut harness, "session", 0, idle);
    assert!(
        matches!(saves(&harness).as_slice(), [(_, id, selection)] if *id > first_id && selection == &desired)
    );
    Ok(())
}
