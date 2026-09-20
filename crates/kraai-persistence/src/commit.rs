use color_eyre::eyre::{Context, Result};
use tokio::sync::OwnedMutexGuard;

pub(crate) async fn complete_commit<T: Send + 'static>(
    guard: OwnedMutexGuard<()>,
    commit: impl Future<Output = Result<T>> + Send + 'static,
    context: &'static str,
) -> Result<T> {
    tokio::spawn(async move {
        let result = commit.await;
        drop(guard);
        result
    })
    .await
    .context(context)?
}
