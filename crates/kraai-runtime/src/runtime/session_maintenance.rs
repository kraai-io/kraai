use super::core::RuntimeCore;
use crate::Event;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio::task::{JoinHandle, JoinSet};

impl RuntimeCore {
    pub(crate) async fn release_turn(&self, session_id: &str) {
        self.agent_manager
            .read()
            .await
            .discard_streaming_state(session_id)
            .await;
        match self.session_store.release_turn(session_id).await {
            Ok(false) => self
                .agent_manager
                .write()
                .await
                .discard_turn_state(session_id),
            Ok(true) => {}
            Err(error) => self.send_session_error(session_id, error),
        }
    }

    pub(crate) async fn finish_turn(&self, session_id: &str) {
        self.agent_manager
            .write()
            .await
            .clear_active_turn(session_id);
        self.release_turn(session_id).await;
        self.event_tx.finish_timer(session_id);
    }

    pub(crate) fn spawn_lease_heartbeat(&self) -> JoinHandle<()> {
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut cancelling = BTreeSet::new();
            let mut cancellations: JoinSet<(String, color_eyre::Result<bool>)> = JoinSet::new();
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
                let sessions = match runtime.session_store.owned_turns().await {
                    Ok(sessions) => sessions,
                    Err(error) => {
                        runtime.send_service_error(error);
                        continue;
                    }
                };
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
                        let cancellation_runtime = runtime.clone();
                        cancellations.spawn(async move {
                            let result = cancellation_runtime
                                .cancel_turn_if_lease_matches(session.clone(), expected)
                                .await;
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
                {
                    let _state_guard = runtime.session_state_barrier.read().await;
                    let agent = runtime.agent_manager.read().await;
                    if let Err(error) = agent.publish_streaming_snapshots().await {
                        runtime.send_service_error(error);
                    }
                }
                let watched: Vec<_> = runtime
                    .observed_sessions
                    .lock()
                    .await
                    .iter()
                    .map(|(session, observation)| (session.clone(), *observation))
                    .collect();
                for (session, previous) in watched {
                    match runtime.session_store.observe(&session).await {
                        Ok(Some(observation))
                            if observation.revision != previous.revision
                                || observation.lease_active != previous.lease_active =>
                        {
                            runtime
                                .observed_sessions
                                .lock()
                                .await
                                .insert(session.clone(), observation);
                            match runtime.session_store.owns_turn(&session).await {
                                Ok(false) => runtime.send_event(Event::HistoryUpdated {
                                    session_id: session,
                                }),
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
