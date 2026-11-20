use super::*;

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression test asserts cleanup state"
)]
fn failed_cleanup_preserves_profile_for_retry() -> Result<(), SandboxError> {
    let mut identity = Identity::create()?;
    let name = identity.name.clone();
    let result = identity.cleanup_with(|_| {
        Err(SandboxError::SandboxUnavailable(
            "injected cleanup failure".into(),
        ))
    });
    assert!(
        matches!(result, Err(SandboxError::SandboxUnavailable(message)) if message == "injected cleanup failure")
    );
    assert_eq!(identity.name, name);
    assert!(identity.alive.is_some());
    identity.cleanup()?;
    assert!(identity.name.is_empty());
    assert!(identity.alive.is_none());
    identity.cleanup_with(|_| {
        Err(SandboxError::SandboxUnavailable(
            "profile deleted twice".into(),
        ))
    })?;
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression test asserts profile lifetimes"
)]
fn launches_get_distinct_identities() -> Result<(), SandboxError> {
    let first = Identity::create()?;
    let second = Identity::create()?;
    assert_ne!(first.name, second.name);
    assert_ne!(first.sid.0, second.sid.0);
    Ok(())
}

fn moniker(identity: &Identity) -> String {
    String::from_utf16_lossy(identity.name.trim_suffix(&[0]))
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression test asserts profile lifetimes"
)]
fn sweep_deletes_only_profiles_without_a_live_launch() -> Result<(), SandboxError> {
    let mut stale = Identity::create()?;
    let live = Identity::create()?;
    let stale_moniker = moniker(&stale);
    let live_moniker = moniker(&live);
    let listed = monikers()?;
    assert!(listed.contains(&stale_moniker));
    assert!(listed.contains(&live_moniker));
    stale.alive = None;
    let stale_name = std::mem::take(&mut stale.name);
    let swept = sweep_stale_profiles();
    let listed = monikers()?;
    if listed.contains(&stale_moniker) {
        delete_profile(&stale_name)?;
    }
    swept?;
    assert!(!listed.contains(&stale_moniker));
    assert!(listed.contains(&live_moniker));
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "regression test asserts capability isolation"
)]
fn path_capabilities_differ_by_path_kind_and_access() -> Result<(), SandboxError> {
    let first = Path::new(r"C:\kraai\first");
    let second = Path::new(r"C:\kraai\second");
    let derived = [
        path_capability("root", first, Access::Read)?,
        path_capability("root", second, Access::Read)?,
        path_capability("ws", first, Access::Read)?,
        path_capability("ws", first, Access::Write)?,
    ]
    .into_iter()
    .flatten()
    .map(|sid| sid.0)
    .collect::<Vec<_>>();
    assert_eq!(derived.len(), 4);
    let distinct = derived.iter().collect::<std::collections::HashSet<_>>();
    assert_eq!(distinct.len(), derived.len());
    let again = path_capability("root", first, Access::Read)?
        .into_iter()
        .map(|sid| sid.0)
        .collect::<Vec<_>>();
    assert_eq!(derived.first(), again.first());
    Ok(())
}
