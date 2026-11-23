use super::short_path;

#[test]
fn only_paths_under_home_are_shortened() {
    assert_eq!(short_path("/home/me/code", "/home/me"), "~/code");
    assert_eq!(short_path("/home/me", "/home/me"), "~");
    assert_eq!(short_path("/home/melon", "/home/me"), "/home/melon");
    assert_eq!(short_path("/tmp/x", "/home/me"), "/tmp/x");
    assert_eq!(short_path("/tmp/x", ""), "/tmp/x");
}
