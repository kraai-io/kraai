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
    let mut pipe = super::client::connect().await.expect("installed helper");
    pipe.write_all(&[0; 20]).await.expect("invalid request");
    assert_ne!(pipe.read_u32_le().await.expect("rejected request"), 0);
    let lease = Lease::acquire(&rand::random())
        .await
        .expect("valid request after rejection");
    lease.release().await.expect("release lease");
}

#[tokio::test]
#[ignore = "terminates the installed service; CI runs this separately after other sandbox tests"]
#[expect(
    unsafe_code,
    reason = "identify the service process for an isolated crash-recovery test"
)]
async fn service_restart_recovers_owned_exemptions() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;

    struct ForeignExemption(String);
    impl Drop for ForeignExemption {
        fn drop(&mut self) {
            let _ = firewall::update(&self.0, false);
        }
    }

    let nonce = rand::random();
    let sid = firewall::profile_sid(&profile_name(&nonce).expect("profile")).expect("SID");
    let lease = Lease::acquire(&nonce).await.expect("active lease");
    let foreign = firewall::profile_sid(&profile_name(&rand::random()).expect("unrelated profile"))
        .expect("unrelated SID");
    firewall::update(&foreign, true).expect("add unrelated exemption");
    let foreign = ForeignExemption(foreign);
    let pipe = super::client::connect().await.expect("service connection");
    let mut pid = 0;
    assert_ne!(
        unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) },
        0
    );
    drop(pipe);
    let killed = std::process::Command::new("taskkill.exe")
        .args(["/F", "/PID", &pid.to_string()])
        .output()
        .expect("terminate service");
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stderr)
    );
    drop(lease);
    assert!(firewall::contains(&sid).expect("journaled exemption survives crash"));
    let started = std::process::Command::new("sc.exe")
        .args(["start", super::SERVICE])
        .output()
        .expect("restart service");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stdout)
    );
    tokio::time::timeout(super::TIMEOUT, async {
        while firewall::contains(&sid).expect("recovery status") {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("restart removes stale owned exemption");
    assert!(firewall::contains(&foreign.0).expect("unrelated exemption is preserved"));
    drop(foreign);
    let lease = Lease::acquire(&rand::random())
        .await
        .expect("service accepts new leases");
    lease.release().await.expect("release lease after recovery");
}
