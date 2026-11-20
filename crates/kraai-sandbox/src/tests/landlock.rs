use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

#[test]
fn host_ipc_restriction_blocks_outside_sockets_and_keeps_new_ones() {
    if crate::ensure_host_ipc_isolation_available().is_err() {
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "tests::landlock::host_ipc_restriction_child",
            "--nocapture",
        ])
        .env("KRAAI_LANDLOCK_TEST_CHILD", "policy")
        .output()
        .expect("run isolated Landlock test");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn host_ipc_restriction_child() {
    if std::env::var("KRAAI_LANDLOCK_TEST_CHILD").as_deref() != Ok("policy") {
        return;
    }
    let dir = super::temp_dir("landlock");
    std::fs::create_dir_all(&dir).expect("create socket directory");
    let outside_path = dir.join("outside.sock");
    let outside = UnixListener::bind(&outside_path).expect("outside socket");
    let abstract_name = format!("kraai-landlock-test-{}", std::process::id());
    let abstract_address =
        SocketAddr::from_abstract_name(abstract_name.as_bytes()).expect("abstract address");
    let _outside_abstract =
        UnixListener::bind_addr(&abstract_address).expect("outside abstract socket");
    let mut existing = UnixStream::connect(&outside_path).expect("trusted startup connection");
    let (mut peer, _) = outside.accept().expect("accept trusted connection");
    for directory in ["from", "to"] {
        std::fs::create_dir(dir.join(directory)).expect("create directory");
    }
    std::fs::write(dir.join("from/file"), "moved").expect("write file");

    crate::restrict_host_ipc_after_startup().expect("install Landlock ruleset");

    std::fs::rename(dir.join("from/file"), dir.join("to/file"))
        .expect("files still move between directories");
    std::fs::hard_link(dir.join("to/file"), dir.join("from/link"))
        .expect("files still link between directories");

    existing
        .write_all(b"ipc")
        .expect("existing connection keeps working");
    let mut bytes = [0; 3];
    peer.read_exact(&mut bytes).expect("read existing traffic");
    assert_eq!(&bytes, b"ipc");

    let denied = UnixStream::connect(&outside_path).expect_err("outside socket is denied");
    assert_eq!(denied.raw_os_error(), Some(libc::EACCES));
    let denied =
        UnixStream::connect_addr(&abstract_address).expect_err("outside abstract is denied");
    assert_eq!(denied.raw_os_error(), Some(libc::EPERM));

    let inside_path = dir.join("inside.sock");
    let inside = UnixListener::bind(&inside_path).expect("socket created after restriction");
    let mut client = UnixStream::connect(&inside_path).expect("new socket stays reachable");
    let (mut server, _) = inside.accept().expect("accept new connection");
    client.write_all(b"new").expect("write to new socket");
    server.read_exact(&mut bytes).expect("read from new socket");
    assert_eq!(&bytes, b"new");
    let _ = std::fs::remove_dir_all(dir);
}
