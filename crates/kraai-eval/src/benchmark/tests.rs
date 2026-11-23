use super::*;

#[test]
fn runner_roots_require_an_actual_store_location() {
    assert!(runner_store_root(Path::new("/tmp/runner")).is_err());
    assert!(runner_store_root(Path::new("/nix/store")).is_err());
    assert!(runner_store_root(Path::new("/nix/store/../secret")).is_err());
    assert!(
        runner_store_root(Path::new("/nix/store/abc-runner/bin/runner")).unwrap()
            == Path::new("/nix/store/abc-runner")
    );
}
