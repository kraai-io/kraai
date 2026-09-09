use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixListener, UnixStream};

#[test]
fn restricted_network_enforces_kernel_policy_and_preserves_subprocess_ipc() {
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "tests::seccomp::restricted_network_child",
            "--nocapture",
        ])
        .env("KRAAI_SECCOMP_TEST_CHILD", "policy")
        .output()
        .expect("run isolated seccomp test");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[expect(
    unsafe_code,
    reason = "kernel integration test exercises socket types and message syscalls directly"
)]
fn restricted_network_child() {
    if std::env::var("KRAAI_SECCOMP_TEST_CHILD").as_deref() != Ok("policy") {
        return;
    }
    let dir = super::temp_dir("seccomp");
    std::fs::create_dir_all(&dir).expect("create socket directory");
    let path = dir.join("host.sock");
    let listener = UnixListener::bind(&path).expect("host socket");
    let mut existing = UnixStream::connect(&path).expect("trusted startup connection");
    let (mut peer, _) = listener.accept().expect("accept trusted connection");

    crate::restrict_network_after_startup().expect("install real seccomp filter");

    existing
        .write_all(b"ipc")
        .expect("existing private connection works");
    let mut bytes = [0; 3];
    peer.read_exact(&mut bytes).expect("read private traffic");
    assert_eq!(&bytes, b"ipc");
    // SAFETY: the address and payload remain live throughout sendto. The policy
    // must reject any explicit destination, including on an existing connection.
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            libc::sendto(
                existing.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(
        UnixDatagram::unbound()
            .expect_err("datagram must be denied")
            .raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(
        UnixStream::connect(&path)
            .expect_err("new connection must be denied")
            .raw_os_error(),
        Some(libc::EPERM)
    );

    let socket = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .expect("standalone stream socket");
    let reused = rustix::io::fcntl_dupfd_cloexec(&socket, 20).expect("reuse startup descriptor");
    assert_eq!(reused.as_raw_fd(), 20);
    let address = rustix::net::SocketAddrUnix::new(&path).expect("host socket address");
    assert_eq!(
        rustix::net::connect(&reused, &address),
        Err(rustix::io::Errno::PERM)
    );

    // Check flag masking and both constructors. A connected datagram pair could
    // otherwise use msg_name to redirect sendmsg to a mounted host endpoint.
    for flags in [
        0,
        libc::SOCK_CLOEXEC,
        libc::SOCK_NONBLOCK,
        libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
    ] {
        let mut pair = [-1; 2];
        // SAFETY: pair points to two writable descriptors; failing calls create none.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_DGRAM | flags,
                    0,
                    pair.as_mut_ptr(),
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
        assert_eq!(
            unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | flags, 0) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        );
    }
    for socket_type in [libc::SOCK_STREAM, libc::SOCK_SEQPACKET] {
        let mut pair = [-1; 2];
        // SAFETY: a successful socketpair initializes both descriptors, which we own.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    socket_type | libc::SOCK_CLOEXEC,
                    0,
                    pair.as_mut_ptr(),
                )
            },
            0
        );
        let left = unsafe { OwnedFd::from_raw_fd(pair[0]) };
        let right = unsafe { OwnedFd::from_raw_fd(pair[1]) };
        let mut data = *b"ok";
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        // SAFETY: a zeroed msghdr has valid null optional pointers. iov and data
        // remain live throughout both synchronous syscalls.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        assert_eq!(unsafe { libc::sendmsg(left.as_raw_fd(), &message, 0) }, 2);
        assert_eq!(
            unsafe { libc::recvmsg(right.as_raw_fd(), &mut message, 0) },
            2
        );
        assert_eq!(&data, b"ok");
    }
    // Exercise Rust's CLOEXEC / pidfd launcher path under the installed filter.
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "tests::seccomp::restricted_network_child"])
        .env("KRAAI_SECCOMP_TEST_CHILD", "subprocess")
        .status()
        .expect("spawn child under restricted policy");
    assert!(status.success());
    std::fs::remove_dir_all(dir).expect("remove socket directory");
}
