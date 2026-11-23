use kraai_test_support::hang_guard;

use super::*;

#[tokio::test]
async fn catalog_replaced_before_forwarding_starts_still_notifies() {
    let (_old, initial) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(initial);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    let forwarding = forward_model_catalog_updates(receiver, events);
    tokio::pin!(forwarding);
    let (_current, replacement) = watch::channel(0);
    catalogs.send_replace(replacement);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(matches!(
        received.try_recv().unwrap().event,
        Event::ModelsUpdated
    ));
    assert!(received.try_recv().is_err());
}

#[tokio::test]
async fn catalog_updates_coalesce_and_follow_only_the_current_source() {
    let (old, initial) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(initial);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    let forwarding = forward_model_catalog_updates(receiver, events);
    tokio::pin!(forwarding);
    old.send_replace(1);
    old.send_replace(2);
    assert!(futures::poll!(&mut forwarding).is_pending());
    let event = received.try_recv().unwrap().event;
    assert!(matches!(event, Event::ModelsUpdated));
    assert!(event.session_id().is_none());
    assert!(received.try_recv().is_err());

    let (current, source) = watch::channel(0);
    catalogs.send_replace(source);
    old.send_replace(3);
    current.send_replace(1);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(matches!(
        received.try_recv().unwrap().event,
        Event::ModelsUpdated
    ));
    assert!(received.try_recv().is_err());
    old.send_replace(4);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(received.try_recv().is_err());

    current.send_replace(2);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(matches!(
        received.try_recv().unwrap().event,
        Event::ModelsUpdated
    ));
    assert!(received.try_recv().is_err());
}

#[tokio::test]
async fn a_closed_catalog_waits_for_replacement_without_spinning() {
    let (old, initial) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(initial);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    let forwarding = forward_model_catalog_updates(receiver, events);
    tokio::pin!(forwarding);
    old.send_replace(1);
    drop(old);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(matches!(
        received.try_recv().unwrap().event,
        Event::ModelsUpdated
    ));
    assert!(received.try_recv().is_err());

    let (current, source) = watch::channel(0);
    catalogs.send_replace(source);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(matches!(
        received.try_recv().unwrap().event,
        Event::ModelsUpdated
    ));
    current.send_replace(1);
    assert!(futures::poll!(&mut forwarding).is_pending());
    assert!(matches!(
        received.try_recv().unwrap().event,
        Event::ModelsUpdated
    ));
    drop(catalogs);
    hang_guard(forwarding).await.unwrap();
}

#[tokio::test]
async fn aborting_catalog_forwarding_releases_its_subscriptions() {
    let (updates, source) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(source);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    updates.send_replace(1);
    let forwarding = tokio::spawn(forward_model_catalog_updates(receiver, events));
    let event = hang_guard(received.recv()).await.unwrap().unwrap();
    assert!(matches!(event.event, Event::ModelsUpdated));
    assert!(updates.receiver_count() == 2);
    forwarding.abort();
    let cancelled = forwarding.await.is_err_and(|error| error.is_cancelled());
    assert!(cancelled);
    assert!(catalogs.receiver_count() == 0);
    assert!(updates.receiver_count() == 1);
    drop(catalogs);
    assert!(updates.receiver_count() == 0);
}
