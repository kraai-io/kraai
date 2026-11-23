use super::*;

#[test]
fn advertised_hosts_support_ipv6_and_reject_ports() {
    assert!(advertised_host("::1").unwrap() == "[::1]");
    assert!(advertised_host("[::1]").unwrap() == "[::1]");
    assert!(advertised_host("127.0.0.1").unwrap() == "127.0.0.1");
    assert!(advertised_host("controller.test").unwrap() == "controller.test");
    for host in [
        "localhost:1000",
        "127.0.0.1:1000",
        "[::1]:1000",
        "",
        "host/path",
        "user@host",
        "[[::1]]",
    ] {
        assert!(advertised_host(host).is_err());
    }
}
