use kraai_types::{ModelOptionValue, ModelOptionValues, ModelSelection, SessionId};

use super::model_options::set as set_option;
use super::*;

mod creation;
mod metadata;

fn configure(harness: &mut TestHarness) {
    model_options::configure(harness);
    harness
        .app
        .reset_chat_session(Some(SessionId::new("session")));
    snapshot(
        harness,
        &SessionId::new("session"),
        0,
        session_snapshot(None),
    );
    harness.drain_requests();
}

fn snapshot(
    harness: &mut TestHarness,
    session_id: &SessionId,
    model_save_id: u64,
    mut snapshot: SessionSnapshot,
) {
    snapshot.session.id = session_id.clone();
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SessionSnapshot {
            session_id: session_id.clone(),
            model_save_id,
            result: Box::new(Ok(snapshot)),
        });
}

fn saves(harness: &TestHarness) -> Vec<(SessionId, u64, ModelSelection)> {
    harness
        .drain_requests()
        .into_iter()
        .filter_map(|request| {
            if let RuntimeRequest::SetSessionModel {
                session_id,
                save_id,
                selection,
            } = request
            {
                Some((session_id, save_id, selection))
            } else {
                None
            }
        })
        .collect()
}

fn acknowledge(harness: &mut TestHarness, session_id: &SessionId, save_id: u64) {
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionModel {
            session_id: session_id.clone(),
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
fn option_picker_saves_partial_choices_before_any_message() {
    let mut harness = test_harness();
    configure(&mut harness);
    harness.app.handle_command("model");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    harness.drain_requests();
    for code in [
        KeyCode::Char('h'),
        KeyCode::Char('i'),
        KeyCode::Char('g'),
        KeyCode::Char('h'),
        KeyCode::Enter,
    ] {
        harness
            .app
            .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let requests = harness.drain_requests();
    assert!(
        matches!(requests.as_slice(), [RuntimeRequest::SetSessionModel { session_id, selection, .. }]
        if session_id.as_str() == "session" && selection.model_id.as_str() == "model" && selection.provider_id.as_str() == "provider"
            && selection.options == ModelOptionValues::from([("effort".into(), ModelOptionValue::Choice("high".into()))]))
    );
    assert!(harness.app.state.sessions.pending_messages.is_empty());
}

#[test]
fn clearing_an_optional_parent_saves_the_map_without_its_dependents() {
    let mut harness = test_harness();
    configure(&mut harness);
    model_options::configure_optional(&mut harness);
    set_option(&mut harness, "thinking", "").unwrap();
    let expected =
        ModelOptionValues::from([("effort".into(), ModelOptionValue::Choice("high".into()))]);
    assert_eq!(harness.app.state.models.selected_options, expected);
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session.as_str() == "session" && selection.options == expected)
    );
}

#[test]
fn model_picker_saves_the_complete_typed_selection() {
    let mut harness = test_harness();
    configure(&mut harness);
    let models = harness
        .app
        .state
        .models
        .by_provider
        .get_mut("provider")
        .expect("missing provider");
    let mut second = models.first().cloned().expect("missing model");
    second.id = "second".into();
    models.push(second);
    let values = options("high", true, Some(50));
    harness.app.state.models.selected_options = options("low", false, None);
    harness
        .app
        .state
        .models
        .remembered_options
        .entry("provider".into())
        .or_default()
        .insert("second".into(), values.clone());
    harness.app.state.mode = UiMode::Model;
    harness.app.state.sheet.index = harness
        .app
        .state
        .filtered_models()
        .iter()
        .position(|(_, model)| model.id == "second")
        .expect("missing second model");
    harness
        .app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(saves(&harness).as_slice(), [(session, _, selection)]
        if session.as_str() == "session" && selection.model_id.as_str() == "second" && selection.options == values)
    );
}

#[test]
fn rapid_edits_coalesce_and_stale_responses_preserve_the_latest_selection() {
    let mut harness = test_harness();
    configure(&mut harness);
    set_option(&mut harness, "effort", "high").unwrap();
    let (_, first_id, first) = saves(&harness)
        .into_iter()
        .next()
        .expect("missing first save");
    for (id, value) in [("effort", "low"), ("thinking", "true"), ("budget", "50")] {
        set_option(&mut harness, id, value).unwrap();
    }
    assert!(saves(&harness).is_empty());
    let desired = options("low", true, Some(50));
    let mut stale = session_snapshot(None);
    stale.session.selected_model = Some(first);
    snapshot(&mut harness, &SessionId::new("session"), 0, stale.clone());
    assert_eq!(harness.app.state.models.selected_options, desired);
    acknowledge(&mut harness, &SessionId::new("session"), first_id);
    let (_, second_id, second) = saves(&harness)
        .into_iter()
        .next()
        .expect("missing coalesced save");
    assert!(second_id > first_id);
    assert_eq!(second.options, desired);
    acknowledge(&mut harness, &SessionId::new("session"), first_id);
    assert!(harness.drain_requests().is_empty());
    acknowledge(&mut harness, &SessionId::new("session"), second_id);
    harness.drain_requests();
    stale.session.tip_id = Some("new-history".into());
    snapshot(&mut harness, &SessionId::new("session"), first_id, stale);
    assert_eq!(harness.app.state.models.selected_options, desired);
    assert_eq!(
        harness.app.state.chat.tip_id.as_deref(),
        Some("new-history")
    );
    let mut confirmed = session_snapshot(None);
    confirmed.session.selected_model = Some(second);
    snapshot(
        &mut harness,
        &SessionId::new("session"),
        second_id,
        confirmed,
    );
    assert_eq!(harness.app.state.models.selected_options, desired);
}

#[test]
fn active_and_queued_turns_defer_saves_and_retain_submitted_options() {
    let mut harness = test_harness();
    configure(&mut harness);
    let submitted = options("high", false, None);
    harness.app.state.models.selected_options = submitted.clone();
    let mut active = session_snapshot(None);
    active.activity = SessionActivity::Streaming;
    active.session.is_running = true;
    active.session.profile_locked = true;
    snapshot(&mut harness, &SessionId::new("session"), 0, active);
    harness.app.submit_message("queued".into());
    set_option(&mut harness, "effort", "low").unwrap();
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::SendMessage { options, .. } if options == &submitted)));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: SessionId::new("session"),
            result: Ok(kraai_runtime::SubmitMessageOutcome::Queued { position: 1 }),
        });
    let mut queued = session_snapshot(None);
    queued.queued_messages = 1;
    snapshot(&mut harness, &SessionId::new("session"), 0, queued);
    assert!(saves(&harness).is_empty());
    snapshot(
        &mut harness,
        &SessionId::new("session"),
        0,
        session_snapshot(None),
    );
    assert!(
        matches!(saves(&harness).as_slice(), [(_, _, selection)] if selection.options == options("low", false, None))
    );
}

#[test]
fn session_switches_keep_deferred_saves_and_ignore_background_acknowledgements() {
    let mut harness = test_harness();
    configure(&mut harness);
    let mut active = session_snapshot(None);
    active.session.is_running = true;
    active.activity = SessionActivity::Streaming;
    snapshot(&mut harness, &SessionId::new("session"), 0, active);
    set_option(&mut harness, "effort", "high").unwrap();
    assert!(saves(&harness).is_empty());
    harness
        .app
        .reset_chat_session(Some(SessionId::new("other")));
    harness.app.state.models.selected_model_id = Some("other-model".into());
    harness.app.state.models.selected_options.clear();
    let idle = session_snapshot(None);
    harness
        .app
        .handle_runtime_response(RuntimeResponse::Sessions(Ok(vec![idle.session.clone()])));
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::GetSessionSnapshot { session_id } if session_id.as_str() == "session")));
    snapshot(&mut harness, &SessionId::new("session"), 0, idle);
    let (_, save_id, selection) = saves(&harness)
        .into_iter()
        .next()
        .expect("missing background save");
    acknowledge(&mut harness, &SessionId::new("session"), save_id);
    assert_eq!(
        harness.app.state.models.selected_model_id.as_deref(),
        Some("other-model")
    );
    assert!(harness.app.state.models.selected_options.is_empty());
    harness
        .app
        .reset_chat_session(Some(SessionId::new("session")));
    let mut restored = session_snapshot(None);
    restored.session.selected_model = Some(selection);
    snapshot(&mut harness, &SessionId::new("session"), save_id, restored);
    assert_eq!(
        harness.app.state.models.selected_options.get("effort"),
        Some(&ModelOptionValue::Choice("high".into()))
    );
}

#[test]
fn admission_failure_refreshes_idle_state_and_flushes_deferred_edits() {
    let mut harness = test_harness();
    configure(&mut harness);
    harness.app.state.models.selected_options = options("high", false, None);
    harness.app.submit_message("message".into());
    set_option(&mut harness, "effort", "low").unwrap();
    assert!(saves(&harness).is_empty());
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SendMessage {
            session_id: SessionId::new("session"),
            result: Err(kraai_runtime::RuntimeError::unavailable("admission failed")),
        });
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::GetSessionSnapshot { session_id } if session_id.as_str() == "session")));
    snapshot(
        &mut harness,
        &SessionId::new("session"),
        0,
        session_snapshot(None),
    );
    assert!(
        matches!(saves(&harness).as_slice(), [(_, _, selection)] if selection.options == options("low", false, None))
    );
}

#[test]
fn transient_save_errors_retry_during_session_reconciliation() {
    let mut harness = test_harness();
    configure(&mut harness);
    set_option(&mut harness, "effort", "high").unwrap();
    let (_, first_id, desired) = saves(&harness)
        .into_iter()
        .next()
        .expect("missing first save");
    harness
        .app
        .handle_runtime_response(RuntimeResponse::SetSessionModel {
            session_id: SessionId::new("session"),
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
    assert!(harness.drain_requests().iter().any(|request| matches!(request, RuntimeRequest::GetSessionSnapshot { session_id } if session_id.as_str() == "session")));
    snapshot(&mut harness, &SessionId::new("session"), 0, idle);
    assert!(
        matches!(saves(&harness).as_slice(), [(_, id, selection)] if *id > first_id && selection == &desired)
    );
}
