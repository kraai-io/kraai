use super::*;
use crate::app::auth::{ProviderAuthState, ProviderAuthStatus};
use crate::app::{ProvidersView, UiMode};
use kraai_runtime::{ProviderAuthStatus as RuntimeProviderAuthStatus, ProviderLoginState};

const PROVIDER: &str = "openai-codex";

fn auth(state: &AppState) -> ProviderAuthStatus {
    state
        .provider_auth
        .get(PROVIDER)
        .cloned()
        .unwrap_or_default()
}

fn auth_event(
    event_sequence: u64,
    sequence: u64,
    state: ProviderLoginState,
) -> RuntimeEventBridgeMessage {
    RuntimeEventBridgeMessage::Event(RuntimeEvent {
        sequence: event_sequence,
        event: Event::ProviderAuthUpdated {
            status: RuntimeProviderAuthStatus {
                provider_type: String::from(PROVIDER),
                sequence,
                state,
                account: None,
                plan: None,
                last_refresh_unix: None,
                error: None,
            },
        },
    })
}

#[test]
fn delayed_auth_snapshot_cannot_restore_completed_login_state() {
    let mut harness = test_harness();
    let (event_tx, event_rx) = unbounded();
    let (response_tx, runtime_rx) = unbounded();
    harness.app.event_rx = event_rx;
    harness.app.runtime_rx = runtime_rx;
    assert!(
        response_tx
            .send(RuntimeResponse::ProviderAuthStatus(Ok(
                ProviderAuthStatus {
                    provider_type: String::from(PROVIDER),
                    sequence: 1,
                    state: ProviderAuthState::BrowserPending,
                    ..Default::default()
                }
            )))
            .is_ok()
    );
    assert!(
        event_tx
            .send(auth_event(1, 2, ProviderLoginState::Authenticated))
            .is_ok()
    );

    assert!(matches!(harness.app.process_events(|| Ok(false)), Ok(true)));

    assert_eq!(
        auth(&harness.app.state).state,
        ProviderAuthState::Authenticated
    );
    assert_eq!(auth(&harness.app.state).sequence, 2);
}

#[test]
fn delayed_auth_event_cannot_overwrite_a_newer_snapshot_or_status() {
    let mut harness = test_harness();
    harness.app.state.mode = UiMode::ProvidersMenu;
    harness.app.state.providers_view = ProvidersView::Detail;
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ProviderAuthStatus(Ok(
            ProviderAuthStatus {
                provider_type: String::from(PROVIDER),
                sequence: 2,
                state: ProviderAuthState::Authenticated,
                ..Default::default()
            },
        )));
    harness.app.state.status = String::from("Current status");

    harness.app.handle_runtime_event_bridge_message(auth_event(
        1,
        1,
        ProviderLoginState::SignedOut,
    ));

    assert_eq!(
        auth(&harness.app.state).state,
        ProviderAuthState::Authenticated
    );
    assert_eq!(auth(&harness.app.state).sequence, 2);
    assert_eq!(harness.app.state.status, "Current status");
}

#[test]
fn current_auth_events_and_responses_apply_in_capture_order() {
    let mut harness = test_harness();
    harness.app.handle_runtime_event_bridge_message(auth_event(
        1,
        1,
        ProviderLoginState::Authenticated,
    ));
    harness
        .app
        .handle_runtime_response(RuntimeResponse::ProviderAuthStatus(Ok(
            ProviderAuthStatus {
                provider_type: String::from(PROVIDER),
                sequence: 2,
                state: ProviderAuthState::SignedOut,
                ..Default::default()
            },
        )));
    assert_eq!(auth(&harness.app.state).state, ProviderAuthState::SignedOut);
    harness.app.handle_runtime_event_bridge_message(auth_event(
        2,
        3,
        ProviderLoginState::Authenticated,
    ));
    assert_eq!(
        auth(&harness.app.state).state,
        ProviderAuthState::Authenticated
    );
    harness.app.handle_runtime_event_bridge_message(auth_event(
        3,
        4,
        ProviderLoginState::SignedOut,
    ));
    assert_eq!(auth(&harness.app.state).state, ProviderAuthState::SignedOut);
    assert_eq!(auth(&harness.app.state).sequence, 4);
}
