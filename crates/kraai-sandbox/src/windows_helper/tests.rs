#![expect(
    clippy::expect_used,
    reason = "verify the installed service manages real Windows exemptions"
)]

use super::{Lease, firewall, profile_name};

#[tokio::test]
async fn exemptions_follow_all_live_leases_and_disconnects() {
    let nonce = rand::random();
    let profile = profile_name(&nonce).expect("derive test profile");
    let sid = firewall::profile_sid(&profile).expect("derive test SID");
    assert!(!firewall::contains(&sid).expect("initial exemptions"));
    let first = Lease::acquire(&nonce).await.expect("installed helper");
    let second = Lease::acquire(&nonce).await.expect("second lease");
    assert!(firewall::contains(&sid).expect("active exemption"));
    first.release().await.expect("release first lease");
    assert!(firewall::contains(&sid).expect("remaining lease retains exemption"));
    second.release().await.expect("release final lease");
    assert!(!firewall::contains(&sid).expect("released exemption"));
    let disconnected = Lease::acquire(&nonce).await.expect("new lease");
    drop(disconnected);
    tokio::time::timeout(super::TIMEOUT, async {
        while firewall::contains(&sid).expect("disconnected exemption") {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("disconnect removes exemption");
}

#[tokio::test]
async fn malformed_requests_do_not_break_subsequent_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut pipe = super::client::connect().expect("installed helper");
    pipe.write_all(&[0; 20]).await.expect("invalid request");
    assert_ne!(pipe.read_u32_le().await.expect("rejected request"), 0);
    let lease = Lease::acquire(&rand::random())
        .await
        .expect("valid request after rejection");
    lease.release().await.expect("release lease");
}
