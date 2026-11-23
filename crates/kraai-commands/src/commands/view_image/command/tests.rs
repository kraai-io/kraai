use super::*;

#[test]
fn reads_binary_and_rejects_non_files_and_oversized_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.png");
    let expected = [0, 255, 128, 13, 10];
    std::fs::write(&path, expected).unwrap();
    assert_eq!(
        read_image(directory.path(), Path::new("image.png")).unwrap(),
        expected
    );
    assert!(read_image(directory.path(), directory.path()).is_err());
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_IMAGE_BYTES as u64 + 1)
        .unwrap();
    assert!(read_image(directory.path(), &path).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_fifo_without_waiting_for_a_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.png");
    nix::unistd::mkfifo(
        &path,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    assert!(read_image(directory.path(), &path).is_err());
}
