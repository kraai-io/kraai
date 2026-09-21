use super::*;
use crate::test_support::test_dir;
use tokio::fs;

#[test]
fn session_temp_write_paths_are_unique_and_adjacent_to_destination() {
    let path = PathBuf::from("/tmp/kraai-data/sessions.json");

    let first = temp_write_path(&path);
    let second = temp_write_path(&path);

    assert_ne!(first, second);
    assert_eq!(first.parent(), path.parent());
    assert_eq!(second.parent(), path.parent());
    assert!(
        first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".kraai-")
    );
    assert!(
        first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".tmp")
    );
    assert!(
        second
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".kraai-")
    );
    assert!(
        second
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".tmp")
    );
}

#[tokio::test]
async fn atomic_write_cleans_temp_file_after_replace_failure() {
    let data_dir = test_dir("atomic-write-cleanup");
    fs::create_dir_all(&data_dir).await.unwrap();
    let destination = data_dir.join("destination");
    fs::create_dir(&destination).await.unwrap();

    let error = atomic_write(&destination, b"contents").await.unwrap_err();

    assert!(error.to_string().contains("Failed to rename temp file"));
    let mut entries = fs::read_dir(&data_dir).await.unwrap();
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    assert_eq!(names, vec![String::from("destination")]);
    let _ = fs::remove_dir_all(&data_dir).await;
}

#[tokio::test]
async fn atomic_write_replaces_file_after_syncing_contents() {
    let data_dir = test_dir("atomic-write-success");
    let destination = data_dir.join("state.json");

    atomic_write(&destination, b"durable").await.unwrap();

    assert_eq!(fs::read(&destination).await.unwrap(), b"durable");
    let _ = fs::remove_dir_all(&data_dir).await;
}

#[cfg(unix)]
#[tokio::test]
async fn atomic_writes_support_a_maximum_length_filename() {
    let data_dir = test_dir("atomic-long-filename");
    fs::create_dir_all(&data_dir).await.unwrap();
    let destination = data_dir.join("x".repeat(255));
    fs::write(&destination, b"original").await.unwrap();

    atomic_write(&destination, b"asynchronous replacement")
        .await
        .unwrap();
    assert_eq!(
        fs::read(&destination).await.unwrap(),
        b"asynchronous replacement"
    );
    atomic_write_sync(&destination, b"synchronous replacement").unwrap();
    assert_eq!(
        fs::read(&destination).await.unwrap(),
        b"synchronous replacement"
    );
    let mut entries = fs::read_dir(&data_dir).await.unwrap();
    assert_eq!(
        entries.next_entry().await.unwrap().unwrap().path(),
        destination
    );
    assert!(entries.next_entry().await.unwrap().is_none());
    fs::remove_dir_all(data_dir).await.unwrap();
}
