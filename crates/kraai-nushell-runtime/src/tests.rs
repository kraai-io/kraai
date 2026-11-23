use super::*;

#[test]
fn host_transport_accepts_standalone_and_internal_invocations() {
    let standalone = ["--transport", "/tmp/host.sock"]
        .into_iter()
        .map(std::ffi::OsString::from);
    assert_eq!(
        host_transport_path_from(standalone),
        Ok(std::path::PathBuf::from("/tmp/host.sock"))
    );

    let internal = [
        INTERNAL_HOST_ARGUMENT,
        "--transport",
        "/tmp/internal-host.sock",
    ]
    .into_iter()
    .map(std::ffi::OsString::from);
    assert_eq!(
        host_transport_path_from(internal),
        Ok(std::path::PathBuf::from("/tmp/internal-host.sock"))
    );
}
