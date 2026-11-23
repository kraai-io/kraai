use std::os::fd::{AsRawFd, IntoRawFd};

use super::{claim_transport_descriptor, transport_socket};

#[test]
fn transport_descriptor_replaces_an_inherited_collision() {
    const CHILD: &str = "KRAAI_TRANSPORT_DESCRIPTOR_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "transport::unix::tests::transport_descriptor_replaces_an_inherited_collision",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        if !output.status.success() {
            panic!(
                "isolated descriptor test failed: {}\nstdout: {}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        return;
    }

    let socket = transport_socket().unwrap();
    let occupied = std::fs::File::open("/dev/null").unwrap();
    let target = occupied.into_raw_fd();
    if socket.as_raw_fd() == target {
        panic!("fixture descriptors collided");
    }

    let transport = claim_transport_descriptor(socket, target).unwrap();

    if transport.as_raw_fd() != target {
        panic!("transport used the wrong descriptor");
    }
    if !rustix::io::fcntl_getfd(&transport)
        .unwrap()
        .contains(rustix::io::FdFlags::CLOEXEC)
    {
        panic!("transport descriptor was inherited across exec");
    }
}
