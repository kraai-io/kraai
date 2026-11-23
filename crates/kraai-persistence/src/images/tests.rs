use super::*;

fn fixture(format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

#[tokio::test]
async fn images_survive_store_recreation() {
    let directory = tempfile::tempdir().unwrap();
    for format in [ImageFormat::Png, ImageFormat::Jpeg] {
        let bytes = fixture(format);
        let store = FileImageStore::new(directory.path());
        let attachment = store.import(bytes.clone()).await.unwrap();
        assert!(attachment.width == 2 && attachment.height == 3);
        assert!(store.import(bytes.clone()).await.unwrap() == attachment);
        drop(store);
        let store = FileImageStore::new(directory.path());
        assert!(store.read(&attachment).await.unwrap() == bytes);
    }
}

#[tokio::test]
async fn rejects_invalid_and_oversized_images() {
    let directory = tempfile::tempdir().unwrap();
    let store = FileImageStore::new(directory.path());
    assert!(store.import(b"not an image".to_vec()).await.is_err());
    assert!(store.import(b"GIF89a".to_vec()).await.is_err());
    assert!(store.import(vec![0; MAX_IMAGE_BYTES + 1]).await.is_err());
    let mut bytes = fixture(ImageFormat::Png);
    bytes.truncate(bytes.len() / 2);
    let result = store.import(bytes).await;
    assert!(result.is_err());
    assert!(!store.directory.exists());
}

#[tokio::test]
async fn rejects_corrupt_bytes_metadata_and_paths() {
    let directory = tempfile::tempdir().unwrap();
    let store = FileImageStore::new(directory.path());
    let attachment = store.import(fixture(ImageFormat::Png)).await.unwrap();
    let mut changed = attachment.clone();
    changed.width += 1;
    assert!(store.read(&changed).await.is_err());
    changed = attachment.clone();
    changed.mime_type = "image/jpeg".to_string();
    assert!(store.read(&changed).await.is_err());
    changed.id = "../outside".to_string();
    assert!(store.read(&changed).await.is_err());
    assert!(
        serde_json::from_value::<ImageAttachment>(serde_json::to_value(&changed).unwrap()).is_err()
    );
    changed = attachment.clone();
    changed.id = "0".repeat(64);
    tokio::fs::copy(
        store.directory.join(&attachment.id),
        store.directory.join(&changed.id),
    )
    .await
    .unwrap();
    assert!(store.read(&changed).await.is_err());
    tokio::fs::write(store.directory.join(&attachment.id), b"corruption")
        .await
        .unwrap();
    assert!(store.read(&attachment).await.is_err());
    tokio::fs::remove_file(store.directory.join(&attachment.id))
        .await
        .unwrap();
    assert!(store.read(&attachment).await.is_err());
}

#[test]
fn rejects_images_above_pixel_limit_before_decoding() {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_luma8(4001, 4000)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    assert!(inspect(bytes.get_ref()).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_fifo_and_symlink_blobs_without_blocking() {
    let directory = tempfile::tempdir().unwrap();
    let store = FileImageStore::new(directory.path());
    let attachment = store.import(fixture(ImageFormat::Png)).await.unwrap();
    let path = store.directory.join(&attachment.id);
    tokio::fs::remove_file(&path).await.unwrap();
    nix::unistd::mkfifo(
        &path,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let read = kraai_test_support::hang_guard(store.read(&attachment))
        .await
        .unwrap();
    assert!(read.is_err());
    tokio::fs::remove_file(&path).await.unwrap();
    let source = directory.path().join("source.png");
    tokio::fs::write(&source, fixture(ImageFormat::Png))
        .await
        .unwrap();
    std::os::unix::fs::symlink(source, path).unwrap();
    assert!(store.read(&attachment).await.is_err());
}
