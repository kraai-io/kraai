use super::retry_exiting_group;
use nix::errno::Errno;

#[test]
fn exiting_group_is_retried_until_it_disappears() {
    let mut calls = 0;
    let result = retry_exiting_group(
        || {
            calls += 1;
            Err(if calls == 1 {
                Errno::EPERM
            } else {
                Errno::ESRCH
            })
        },
        || {},
    );
    assert_eq!(result, Err(Errno::ESRCH));
    assert_eq!(calls, 2);
}

#[test]
fn persistent_permission_errors_remain_errors() {
    let mut pauses = 0;
    assert_eq!(
        retry_exiting_group(|| Err(Errno::EPERM), || pauses += 1),
        Err(Errno::EPERM)
    );
    assert_eq!(pauses, 10);
}
