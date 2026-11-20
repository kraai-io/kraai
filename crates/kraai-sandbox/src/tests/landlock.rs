use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

fn run_child(test: &str, mode: &str, moves: bool) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", test, "--nocapture"])
        .env("KRAAI_LANDLOCK_TEST_CHILD", mode)
        .env("KRAAI_LANDLOCK_MOVES", if moves { "1" } else { "0" })
        .output()
        .expect("run isolated Landlock test")
}

fn move_between(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(dir.join("from/file"), dir.join("to/file"))?;
    std::fs::hard_link(dir.join("to/file"), dir.join("from/link"))
}

fn prepare_moves(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("create test directory");
    for directory in ["from", "to"] {
        std::fs::create_dir(dir.join(directory)).expect("create directory");
    }
    std::fs::write(dir.join("from/file"), "moved").expect("write file");
}

#[test]
fn host_ipc_restriction_blocks_outside_sockets_and_keeps_new_ones() {
    if crate::ensure_host_ipc_isolation_available().is_err() {
        return;
    }
    // An enclosing Landlock layer that handles no filesystem rights, like the Nix build
    // sandbox's, denies moves between directories once any layer handles one.
    let moves = run_child("tests::landlock::moves_child", "moves", false)
        .status
        .success();
    let output = run_child(
        "tests::landlock::host_ipc_restriction_child",
        "policy",
        moves,
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn moves_child() {
    if std::env::var("KRAAI_LANDLOCK_TEST_CHILD").as_deref() != Ok("moves") {
        return;
    }
    let dir = super::temp_dir("landlock-moves");
    prepare_moves(&dir);
    crate::restrict_only_moves().expect("install Landlock ruleset");
    let moved = move_between(&dir);
    let _ = std::fs::remove_dir_all(dir);
    moved.expect("move between directories");
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
    prepare_moves(&dir.join("moves"));

    crate::restrict_host_ipc_after_startup().expect("install Landlock ruleset");

    if std::env::var("KRAAI_LANDLOCK_MOVES").as_deref() == Ok("1") {
        move_between(&dir.join("moves")).expect("files still move between directories");
    }

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
