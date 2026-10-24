use super::*;
use crate::Persistence;
use color_eyre::eyre::ensure;
use serde_json::json;

fn version(source: &str, fetched_at: i64) -> ModelListVersion {
    ModelListVersion {
        source: source.into(),
        fetched_at,
    }
}

#[tokio::test]
async fn claims_are_exclusive_until_they_expire() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = Persistence::open(directory.path()).await?;
    let second = Persistence::open(directory.path()).await?;
    let provider = ProviderId::new("provider");

    ensure!(first.model_lists().claim(&provider, None, 100, 200).await?);
    ensure!(
        !second
            .model_lists()
            .claim(&provider, None, 150, 250)
            .await?
    );
    ensure!(!first.model_lists().claim(&provider, None, 199, 299).await?);
    ensure!(
        second
            .model_lists()
            .claim(&provider, None, 200, 300)
            .await?
    );
    first.model_lists().release(&provider, 200).await?;
    ensure!(!first.model_lists().claim(&provider, None, 250, 350).await?);
    second.model_lists().release(&provider, 300).await?;
    ensure!(first.model_lists().claim(&provider, None, 250, 350).await?);
    ensure!(
        first
            .model_lists()
            .claim(&ProviderId::new("other"), None, 150, 250)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn saving_publishes_the_list_and_only_its_readers_can_claim_it() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Persistence::open(directory.path())
        .await?
        .model_lists()
        .clone();
    let provider = ProviderId::new("provider");

    ensure!(store.claim(&provider, None, 100, 200).await?);
    store
        .save(
            &provider,
            &version("https://api.test", 120),
            &json!({"data": []}),
        )
        .await?;
    ensure!(!store.claim(&provider, None, 130, 230).await?);
    ensure!(
        !store
            .claim(&provider, Some(&version("https://api.test", 110)), 130, 230)
            .await?
    );
    ensure!(
        store
            .claim(&provider, Some(&version("https://api.test", 120)), 130, 230)
            .await?
    );
    ensure!(
        store.read(&provider).await?
            == Some(StoredModelList {
                version: version("https://api.test", 120),
                data: json!({"data": []}),
            })
    );

    store
        .save(
            &provider,
            &version("https://other.test", 140),
            &json!({"data": [1]}),
        )
        .await?;
    ensure!(
        store.versions().await?
            == BTreeMap::from([(provider.clone(), version("https://other.test", 140))])
    );
    Ok(())
}

#[tokio::test]
async fn a_list_fetched_earlier_never_replaces_a_later_one() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Persistence::open(directory.path())
        .await?
        .model_lists()
        .clone();
    let provider = ProviderId::new("provider");

    store
        .save(&provider, &version("https://api.test", 140), &json!([2]))
        .await?;
    store
        .save(&provider, &version("https://api.test", 120), &json!([1]))
        .await?;
    ensure!(
        store.read(&provider).await?
            == Some(StoredModelList {
                version: version("https://api.test", 140),
                data: json!([2]),
            })
    );
    Ok(())
}

#[tokio::test]
async fn claimed_rows_without_data_are_not_listed() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Persistence::open(directory.path())
        .await?
        .model_lists()
        .clone();
    let claimed = ProviderId::new("claimed");

    ensure!(store.claim(&claimed, None, 100, 200).await?);
    ensure!(store.versions().await?.is_empty());
    ensure!(store.read(&claimed).await?.is_none());
    Ok(())
}
