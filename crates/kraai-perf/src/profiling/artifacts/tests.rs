use super::*;

#[test]
fn publishing_does_not_replace_an_existing_directory() {
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("profile");
    fs::create_dir(&output).unwrap();
    let staging = tempfile::tempdir_in(parent.path()).unwrap();
    fs::write(staging.path().join("profile.json"), b"new profile").unwrap();
    let result = publish(staging, &output);
    assert!(result.is_err());
    assert!(output.read_dir().unwrap().next().is_none());
}
