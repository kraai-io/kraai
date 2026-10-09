use color_eyre::eyre::{Result, ensure};

use super::*;

#[tokio::test]
async fn catalog_replaced_before_forwarding_starts_still_notifies() -> Result<()> {
    let (_old, initial) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(initial);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    let forwarding = forward_model_catalog_updates(receiver, events);
    tokio::pin!(forwarding);
    let (_current, replacement) = watch::channel(0);
    catalogs.send_replace(replacement);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(matches!(received.try_recv()?.event, Event::ModelsUpdated));
    ensure!(received.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn catalog_updates_coalesce_and_follow_only_the_current_source() -> Result<()> {
    let (old, initial) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(initial);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    let forwarding = forward_model_catalog_updates(receiver, events);
    tokio::pin!(forwarding);
    old.send_replace(1);
    old.send_replace(2);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    let event = received.try_recv()?.event;
    ensure!(matches!(event, Event::ModelsUpdated));
    ensure!(event.session_id().is_none());
    ensure!(received.try_recv().is_err());

    let (current, source) = watch::channel(0);
    catalogs.send_replace(source);
    old.send_replace(3);
    current.send_replace(1);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(matches!(received.try_recv()?.event, Event::ModelsUpdated));
    ensure!(received.try_recv().is_err());
    old.send_replace(4);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(received.try_recv().is_err());

    current.send_replace(2);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(matches!(received.try_recv()?.event, Event::ModelsUpdated));
    ensure!(received.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn a_closed_catalog_waits_for_replacement_without_spinning() -> Result<()> {
    let (old, initial) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(initial);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    let forwarding = forward_model_catalog_updates(receiver, events);
    tokio::pin!(forwarding);
    old.send_replace(1);
    drop(old);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(matches!(received.try_recv()?.event, Event::ModelsUpdated));
    ensure!(received.try_recv().is_err());

    let (current, source) = watch::channel(0);
    catalogs.send_replace(source);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(matches!(received.try_recv()?.event, Event::ModelsUpdated));
    current.send_replace(1);
    ensure!(futures::poll!(&mut forwarding).is_pending());
    ensure!(matches!(received.try_recv()?.event, Event::ModelsUpdated));
    drop(catalogs);
    tokio::time::timeout(std::time::Duration::from_secs(1), forwarding).await?;
    Ok(())
}

#[tokio::test]
async fn aborting_catalog_forwarding_releases_its_subscriptions() -> Result<()> {
    let (updates, source) = watch::channel(0);
    let (catalogs, receiver) = watch::channel(source);
    let events = RuntimeEventSender::new(8);
    let mut received = events.subscribe();
    updates.send_replace(1);
    let forwarding = tokio::spawn(forward_model_catalog_updates(receiver, events));
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), received.recv()).await??;
    ensure!(matches!(event.event, Event::ModelsUpdated));
    ensure!(updates.receiver_count() == 2);
    forwarding.abort();
    let cancelled = forwarding.await.is_err_and(|error| error.is_cancelled());
    ensure!(cancelled);
    ensure!(catalogs.receiver_count() == 0);
    ensure!(updates.receiver_count() == 1);
    drop(catalogs);
    ensure!(updates.receiver_count() == 0);
    Ok(())
}
