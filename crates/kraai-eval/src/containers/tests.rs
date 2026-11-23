use super::*;

#[test]
fn container_addresses_must_be_reachable() {
    for invalid in ["", "127.0.0.1", "::1", "0.0.0.0", "::"] {
        assert!(reachable_address(invalid).is_err());
    }
    assert!(reachable_address("169.254.1.2").unwrap() == "169.254.1.2");
}
