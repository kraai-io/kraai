use super::core::RuntimeCore;
use crate::{Event, RuntimeEvent};
use kraai_types::SessionId;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};

impl RuntimeCore {
    /// Releases the session's turn, saving its timer stopped so other runtimes never show a
    /// turn without an owner as running.
    pub(crate) async fn release_turn(&self, session_id: &SessionId) {
        self.agent_manager.discard_streaming_state(session_id).await;
        let mut timer = self.event_tx.timer_snapshot(session_id).1;
        timer.finish(std::time::Instant::now());
        let released = match serde_json::to_string(&timer) {
            Ok(timer) => {
                self.session_store
                    .release_turn_with_timer(session_id, timer)
                    .await
            }
            Err(error) => Err(error.into()),
        };
        match released {
            Ok(false) => self.agent_manager.clear_active_turn(session_id),
            Ok(true) => {}
            Err(error) => self.send_session_error(session_id, error),
        }
    }

    pub(crate) async fn finish_turn(&self, session_id: &SessionId) {
        self.agent_manager.clear_active_turn(session_id);
        self.event_tx.finish_timer(session_id);
        self.release_turn(session_id).await;
    }

    /// Saves the timer of every turn this runtime runs whenever it changes.
    pub(crate) fn spawn_turn_timer_persistence(&self) -> JoinHandle<()> {
        let runtime = self.clone();
        let mut events = self.event_tx.subscribe();
        tokio::spawn(async move {
            loop {
                let sessions = match events.recv().await {
                    Ok(RuntimeEvent {
                        event: Event::TurnTimingChanged { session_id, .. },
                        ..
                    }) => vec![session_id],
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        runtime.session_store.owned_sessions()
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                for session_id in sessions {
                    let timer = runtime.event_tx.timer_snapshot(&session_id).1;
                    let saved = match serde_json::to_string(&timer) {
                        Ok(timer) => {
                            runtime
                                .session_store
                                .save_turn_timer(&session_id, timer)
                                .await
                        }
                        Err(error) => Err(error.into()),
                    };
                    if let Err(error) = saved {
                        tracing::warn!(%session_id, error = %format!("{error:#}"), "Unable to save turn timer");
                    }
                }
            }
        })
    }

    pub(crate) fn spawn_lease_heartbeat(&self) -> JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut cancelling = BTreeSet::new();
            let mut cancellations: JoinSet<(SessionId, color_eyre::Result<bool>)> = JoinSet::new();
            loop {
                tokio::select! {
                    _ = interval.tick() => {},
                    Some(completed) = cancellations.join_next(), if !cancellations.is_empty() => {
                        match completed {
                            Ok((session, result)) => {
                                cancelling.remove(&session);
                                if let Err(error) = result {
                                    runtime.send_session_error(&session, error);
                                }
                            }
                            Err(error) => runtime.send_service_error(error),
                        }
                        continue;
                    }
                }
                let sessions = runtime.session_store.owned_turns();
                for (session, expected) in sessions {
                    if cancelling.contains(&session) {
                        continue;
                    }
                    if let Err(error) = runtime.session_store.renew_turn(&session).await {
                        runtime.send_session_error(
                            &session,
                            format!("Session lease renewal failed: {error}"),
                        );
                        cancelling.insert(session.clone());
                        let (response, cancelled) = tokio::sync::oneshot::channel();
                        runtime.sessions.send(
                            &runtime,
                            &session,
                            super::session::SessionMessage::Cancel {
                                discard_queue: true,
                                expected_lease: Some(expected),
                                response: Some(response),
                            },
                        );
                        cancellations.spawn(async move {
                            let result = match cancelled.await {
                                Ok(result) => {
                                    result.map_err(|error| color_eyre::eyre::eyre!(error))
                                }
                                Err(_) => Ok(false),
                            };
                            (session, result)
                        });
                    }
                }
            }
        })
    }

    pub(crate) fn spawn_session_observers(&self) -> JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error) = runtime.agent_manager.publish_streaming_snapshots().await {
                    runtime.send_service_error(error);
                }
                let watched: Vec<_> = runtime
                    .observed_sessions
                    .lock()
                    .await
                    .iter()
                    .map(|(session, observation)| (session.clone(), observation.clone()))
                    .collect();
                for (session, previous) in watched {
                    match runtime.session_store.observe(&session).await {
                        Ok(Some(observation))
                            if observation.revision != previous.revision
                                || observation.lease_active != previous.lease_active
                                || observation.turn_timer != previous.turn_timer =>
                        {
                            runtime
                                .observed_sessions
                                .lock()
                                .await
                                .insert(session.clone(), observation.clone());
                            match runtime.session_store.owns_turn(&session).await {
                                Ok(false) => {
                                    if observation.turn_timer != previous.turn_timer
                                        && let Some(timer) =
                                            stored_turn_timer(&session, &observation)
                                    {
                                        runtime.send_event(Event::TurnTimingChanged {
                                            session_id: session.clone(),
                                            timer,
                                        });
                                    }
                                    if observation.revision != previous.revision
                                        || observation.lease_active != previous.lease_active
                                    {
                                        runtime.send_event(Event::HistoryUpdated {
                                            session_id: session,
                                        });
                                    }
                                }
                                Err(error) => runtime.send_session_error(&session, error),
                                _ => {}
                            }
                        }
                        Ok(None) => {
                            runtime.observed_sessions.lock().await.remove(&session);
                            runtime.send_event(Event::HistoryUpdated {
                                session_id: session,
                            });
                        }
                        Err(error) => runtime.send_session_error(&session, error),
                        _ => {}
                    }
                }
            }
        })
    }
}

impl RuntimeCore {
    /// Recovers sessions left mid-turn by a runtime that stopped without releasing them.
    pub(crate) async fn sweep_expired_leases(&self) {
        let sessions = match self.session_store.expired_leases().await {
            Ok(sessions) => sessions,
            Err(error) => {
                self.send_service_error(error);
                return;
            }
        };
        for session_id in sessions {
            if self.session_store.claim_turn(&session_id).await.is_err() {
                continue;
            }
            let recovered = async {
                self.recover_session_executions(&session_id).await?;
                self.agent_manager
                    .recover_interrupted_session(&session_id)
                    .await
            }
            .await;
            if let Err(error) = recovered {
                self.send_session_report_error(&session_id, error);
            }
            self.release_turn(&session_id).await;
            self.send_event(Event::HistoryUpdated { session_id });
        }
    }
}

/// The turn timer another runtime saved for the session, if it saved a readable one.
pub(crate) fn stored_turn_timer(
    session_id: &SessionId,
    observation: &kraai_persistence::SessionObservation,
) -> Option<crate::TurnTimer> {
    let timer = observation.turn_timer.as_deref()?;
    serde_json::from_str(timer)
        .inspect_err(|error| tracing::warn!(%session_id, %error, "Ignoring unreadable turn timer"))
        .ok()
}
