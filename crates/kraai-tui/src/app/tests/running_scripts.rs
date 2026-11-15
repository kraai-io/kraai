use std::time::{Duration, Instant};

use kraai_execution_types::{ScriptExecutionOutcome, ScriptExecutionStatus, ScriptOutputStream};
use kraai_runtime::ScriptOutputLine;
use kraai_types::SessionId;
use ratatui::style::Color;

use super::{Event, ScriptPhase, TestHarness, pending_script, test_harness};
use crate::components::{RunningCard, ScriptCard, running_dot};

fn started(harness: &mut TestHarness, session: &str, call: &str) {
    harness.app.handle_runtime_event(Event::ScriptStarted {
        session_id: SessionId::new(session),
        call_id: call.to_owned(),
        timeout_millis: 300_000,
    });
}

fn tail(harness: &mut TestHarness, call: &str, lines: &[&str]) {
    harness.app.handle_runtime_event(Event::ScriptOutputTail {
        session_id: SessionId::new("session"),
        call_id: call.to_owned(),
        lines: lines.iter().map(|text| output(text)).collect(),
    });
}

fn output(text: &str) -> ScriptOutputLine {
    ScriptOutputLine {
        stream: ScriptOutputStream::Stdout,
        text: text.to_owned(),
    }
}

fn running(elapsed: Duration, tail: &[&str]) -> ScriptCard {
    running_on(elapsed, tail, None)
}

fn running_on(elapsed: Duration, tail: &[&str], muted: Option<Color>) -> ScriptCard {
    ScriptCard::Running(RunningCard::new(
        elapsed,
        Duration::from_secs(300),
        tail.iter().map(|text| output(text)).collect(),
        running_dot(elapsed, muted),
    ))
}

fn clocked_harness() -> (TestHarness, Instant) {
    let mut harness = session_harness();
    started(&mut harness, "session", "call");
    let start = Instant::now();
    if let Some(script) = harness.app.state.running_scripts.get_mut("call") {
        script.started = start;
    }
    harness.app.state.script_clock = Some(start);
    harness.app.state.breath_origin = start;
    (harness, start)
}

fn session_harness() -> TestHarness {
    let mut harness = test_harness();
    harness.app.state.current_session_id = Some(SessionId::new("session"));
    harness
}

#[test]
fn a_started_script_runs_with_its_timeout_and_its_latest_tail() {
    let mut harness = session_harness();
    assert_eq!(
        harness.app.state.script_card("call", false),
        ScriptCard::Waiting
    );
    assert_eq!(
        harness.app.state.script_card("call", true),
        ScriptCard::Writing
    );

    started(&mut harness, "other", "call");
    assert!(harness.app.state.running_scripts.is_empty());

    let epoch = harness.app.state.cards_epoch;
    started(&mut harness, "session", "call");
    assert_ne!(harness.app.state.cards_epoch, epoch);
    assert_eq!(
        harness.app.state.script_card("call", true),
        running(Duration::ZERO, &[])
    );

    tail(&mut harness, "call", &["one", "two"]);
    tail(&mut harness, "call", &["two", "three"]);
    tail(&mut harness, "unknown", &["ignored"]);
    assert_eq!(
        harness.app.state.script_card("call", false),
        running(Duration::ZERO, &["two", "three"])
    );
    assert_eq!(
        harness.app.state.script_card("unknown", false),
        ScriptCard::Waiting
    );
}

#[test]
fn the_card_waits_for_approval_only_for_the_call_that_asked() {
    let mut harness = session_harness();
    let mut script = pending_script("execution");
    script.call_id = String::from("call");
    harness.app.state.pending_script = Some(script);
    harness.app.state.script_phase = ScriptPhase::AwaitingApproval;
    assert_eq!(
        harness.app.state.script_card("call", false),
        ScriptCard::AwaitingApproval
    );
    assert_eq!(
        harness.app.state.script_card("other", false),
        ScriptCard::Waiting
    );
}

#[test]
fn the_clock_redraws_the_chat_only_when_a_card_would_change() {
    let (mut harness, start) = clocked_harness();
    let at = |millis: u64| start + Duration::from_millis(millis);

    let epoch = harness.app.state.cards_epoch;
    assert!(!harness.app.advance_script_clock(at(499)));
    assert_eq!(harness.app.state.cards_epoch, epoch);
    assert!(harness.app.advance_script_clock(at(1700)));
    assert_ne!(harness.app.state.cards_epoch, epoch);
    assert_eq!(
        harness.app.state.script_card("call", false),
        running(Duration::from_millis(1700), &[])
    );
    assert_eq!(
        harness.app.state.script_card("call", false),
        ScriptCard::Running(RunningCard {
            elapsed: Duration::from_secs(1),
            dot: crate::theme::MUTED,
            timeout: Duration::from_secs(300),
            tail: Vec::new(),
        })
    );

    let epoch = harness.app.state.cards_epoch;
    assert!(!harness.app.advance_script_clock(at(1999)));
    assert_eq!(harness.app.state.cards_epoch, epoch);
    assert!(harness.app.advance_script_clock(at(2000)));
    assert_ne!(harness.app.state.cards_epoch, epoch);
}

#[test]
fn the_clock_redraws_a_fading_dot_once_a_frame_and_only_when_its_colour_moves() {
    let (mut harness, start) = clocked_harness();
    let muted = Some(Color::Rgb(88, 82, 115));
    harness.app.state.palette.muted = muted;
    let at = |millis: u64| start + Duration::from_millis(millis);

    let epoch = harness.app.state.cards_epoch;
    for millis in [16, 49] {
        assert!(!harness.app.advance_script_clock(at(millis)), "{millis}");
    }
    assert_eq!(harness.app.state.cards_epoch, epoch);
    assert_eq!(harness.app.state.script_clock, Some(start));

    assert!(harness.app.advance_script_clock(at(800)));
    assert_ne!(harness.app.state.cards_epoch, epoch);
    assert_eq!(
        harness.app.state.script_card("call", false),
        running_on(Duration::from_millis(800), &[], muted)
    );
    assert_eq!(
        harness.app.state.script_card("call", false),
        ScriptCard::Running(RunningCard {
            elapsed: Duration::ZERO,
            dot: Color::Rgb(101, 97, 129),
            timeout: Duration::from_secs(300),
            tail: Vec::new(),
        })
    );
    assert!(!harness.app.advance_script_clock(at(849)));
    assert!(harness.app.advance_script_clock(at(850)));

    let redraws = (851..=3_250)
        .filter(|millis| harness.app.advance_script_clock(at(*millis)))
        .count();
    assert!(redraws <= 48, "{redraws}");
    assert!(redraws >= 40, "{redraws}");
}

#[test]
fn a_result_a_finished_turn_or_another_session_ends_the_running_card() {
    let mut harness = session_harness();
    started(&mut harness, "session", "call");
    harness.app.handle_runtime_event(Event::ScriptResultReady {
        session_id: SessionId::new("session"),
        execution_id: String::from("execution"),
        call_id: String::from("call"),
        output: Default::default(),
        outcome: ScriptExecutionOutcome {
            status: ScriptExecutionStatus::Completed,
            exit_code: Some(0),
        },
    });
    assert!(harness.app.state.running_scripts.is_empty());
    assert!(!harness.app.advance_script_clock(Instant::now()));
    assert_eq!(harness.app.state.script_clock, None);

    started(&mut harness, "session", "call");
    harness.app.handle_runtime_event(Event::TurnCompleted {
        session_id: SessionId::new("session"),
    });
    assert!(harness.app.state.running_scripts.is_empty());

    started(&mut harness, "session", "call");
    harness
        .app
        .reset_session_state(Some(SessionId::new("another")));
    assert!(harness.app.state.running_scripts.is_empty());
}

#[test]
fn a_finished_turn_redraws_the_cards_it_stops() {
    let mut harness = session_harness();
    started(&mut harness, "session", "call");
    let epoch = harness.app.state.cards_epoch;
    harness.app.handle_runtime_event(Event::TurnCompleted {
        session_id: SessionId::new("session"),
    });
    assert!(harness.app.state.running_scripts.is_empty());
    assert_ne!(harness.app.state.cards_epoch, epoch);
}

#[test]
fn a_snapshot_that_runs_no_script_drops_the_running_cards() {
    for (activity, keeps) in [
        (kraai_runtime::SessionActivity::ExecutingScript, true),
        (kraai_runtime::SessionActivity::Idle, false),
    ] {
        let mut harness = session_harness();
        started(&mut harness, "session", "call");
        let mut snapshot = super::session_snapshot_at(1, None);
        snapshot.activity = activity;
        harness
            .app
            .handle_runtime_response(super::RuntimeResponse::SessionSnapshot {
                model_save_id: 0,
                session_id: SessionId::new("session"),
                result: Box::new(Ok(snapshot)),
            });
        assert_eq!(
            harness.app.state.running_scripts.contains_key("call"),
            keeps,
            "{activity:?}"
        );
    }
}
