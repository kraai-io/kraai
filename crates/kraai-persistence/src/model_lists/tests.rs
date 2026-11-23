use super::*;
use crate::Persistence;
use serde_json::json;

fn version(source: &str, fetched_at: i64) -> ModelListVersion {
    ModelListVersion {
        source: source.into(),
        fetched_at,
    }
}

#[tokio::test]
async fn claims_are_exclusive_until_they_expire() {
    let directory = tempfile::tempdir().unwrap();
    let first = Persistence::open(directory.path()).await.unwrap();
    let second = Persistence::open(directory.path()).await.unwrap();
    let provider = ProviderId::new("provider");

    assert!(
        first
            .model_lists()
            .claim(&provider, None, 100, 200)
            .await
            .unwrap()
    );
    assert!(
        !second
            .model_lists()
            .claim(&provider, None, 150, 250)
            .await
            .unwrap()
    );
    assert!(
        !first
            .model_lists()
            .claim(&provider, None, 199, 299)
            .await
            .unwrap()
    );
    assert!(
        second
            .model_lists()
            .claim(&provider, None, 200, 300)
            .await
            .unwrap()
    );
    first.model_lists().release(&provider, 200).await.unwrap();
    assert!(
        !first
            .model_lists()
            .claim(&provider, None, 250, 350)
            .await
            .unwrap()
    );
    second.model_lists().release(&provider, 300).await.unwrap();
    assert!(
        first
            .model_lists()
            .claim(&provider, None, 250, 350)
            .await
            .unwrap()
    );
    assert!(
        first
            .model_lists()
            .claim(&ProviderId::new("other"), None, 150, 250)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn saving_publishes_the_list_and_only_its_readers_can_claim_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = Persistence::open(directory.path())
        .await
        .unwrap()
        .model_lists()
        .clone();
    let provider = ProviderId::new("provider");

    assert!(store.claim(&provider, None, 100, 200).await.unwrap());
    store
        .save(
            &provider,
            200,
            &version("https://api.test", 120),
            &json!({"data": []}),
        )
        .await
        .unwrap();
    store.release(&provider, 200).await.unwrap();
    assert!(!store.claim(&provider, None, 130, 230).await.unwrap());
    assert!(
        !store
            .claim(&provider, Some(&version("https://api.test", 110)), 130, 230)
            .await
            .unwrap()
    );
    assert!(
        store
            .claim(&provider, Some(&version("https://api.test", 120)), 130, 230)
            .await
            .unwrap()
    );
    assert!(
        store.read(&provider).await.unwrap()
            == Some(StoredModelList {
                version: version("https://api.test", 120),
                data: json!({"data": []}),
            })
    );

    store
        .save(
            &provider,
            230,
            &version("https://other.test", 140),
            &json!({"data": [1]}),
        )
        .await
        .unwrap();
    assert!(
        store.versions().await.unwrap()
            == BTreeMap::from([(provider.clone(), version("https://other.test", 140))])
    );
}

#[tokio::test]
async fn only_the_claim_holder_saves_and_each_save_moves_the_version_forward() {
    let directory = tempfile::tempdir().unwrap();
    let store = Persistence::open(directory.path())
        .await
        .unwrap()
        .model_lists()
        .clone();
    let provider = ProviderId::new("provider");

    assert!(
        store
            .save(&provider, 0, &version("https://api.test", 100), &json!([0]))
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.claim(&provider, None, 100, 200).await.unwrap());
    assert!(
        store
            .save(
                &provider,
                200,
                &version("https://api.test", 140),
                &json!([2])
            )
            .await
            .unwrap()
            == Some(version("https://api.test", 140))
    );
    assert!(
        store
            .save(
                &provider,
                200,
                &version("https://api.test", 120),
                &json!([1])
            )
            .await
            .unwrap()
            == Some(version("https://api.test", 141))
    );
    assert!(store.renew(&provider, 200, 300).await.unwrap());
    assert!(
        store
            .save(
                &provider,
                200,
                &version("https://api.test", 150),
                &json!([3])
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store.read(&provider).await.unwrap()
            == Some(StoredModelList {
                version: version("https://api.test", 141),
                data: json!([1]),
            })
    );
}

#[tokio::test]
async fn claimed_rows_without_data_are_not_listed() {
    let directory = tempfile::tempdir().unwrap();
    let store = Persistence::open(directory.path())
        .await
        .unwrap()
        .model_lists()
        .clone();
    let claimed = ProviderId::new("claimed");

    assert!(store.claim(&claimed, None, 100, 200).await.unwrap());
    assert!(store.versions().await.unwrap().is_empty());
    assert!(store.read(&claimed).await.unwrap().is_none());
}

#[tokio::test]
async fn any_version_claims_wait_only_for_a_live_claim() {
    let directory = tempfile::tempdir().unwrap();
    let store = Persistence::open(directory.path())
        .await
        .unwrap()
        .model_lists()
        .clone();
    let provider = ProviderId::new("provider");

    assert!(store.claim_any_version(&provider, 100, 200).await.unwrap());
    assert!(!store.claim_any_version(&provider, 150, 250).await.unwrap());
    store
        .save(
            &provider,
            200,
            &version("https://api.test", 160),
            &json!([]),
        )
        .await
        .unwrap();
    assert!(!store.claim_any_version(&provider, 170, 270).await.unwrap());
    store.release(&provider, 200).await.unwrap();
    assert!(store.claim_any_version(&provider, 170, 270).await.unwrap());
    assert!(!store.claim(&provider, None, 200, 300).await.unwrap());
    store.release(&provider, 270).await.unwrap();
    assert!(store.claim_any_version(&provider, 200, 300).await.unwrap());
}

#[tokio::test]
async fn only_the_current_holder_can_renew_a_claim() {
    let directory = tempfile::tempdir().unwrap();
    let store = Persistence::open(directory.path())
        .await
        .unwrap()
        .model_lists()
        .clone();
    let provider = ProviderId::new("provider");

    assert!(store.claim(&provider, None, 100, 200).await.unwrap());
    assert!(store.renew(&provider, 200, 300).await.unwrap());
    assert!(!store.claim_any_version(&provider, 250, 350).await.unwrap());
    assert!(!store.renew(&provider, 200, 400).await.unwrap());
    assert!(store.claim_any_version(&provider, 300, 400).await.unwrap());
    assert!(!store.renew(&provider, 300, 500).await.unwrap());
    store.release(&provider, 300).await.unwrap();
    assert!(!store.claim_any_version(&provider, 350, 450).await.unwrap());
}
