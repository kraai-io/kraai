use super::*;

fn paths(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

fn grants(grants: &[(&str, bool)]) -> Vec<(PathBuf, bool)> {
    grants
        .iter()
        .map(|(path, writable)| (PathBuf::from(path), *writable))
        .collect()
}

#[test]
fn denied_paths_outside_every_grant_need_no_entries() {
    let denials = Denials::new(
        paths(&["/home/.kraai", "/home/.kraai/eval"]),
        grants(&[("/home/.kraai/eval/workspace", true), ("/work", true)]),
    );
    assert!(denials.roots().is_empty());
}

#[test]
fn grants_inside_a_denied_path_keep_their_access() {
    let denials = Denials::new(
        paths(&["/home/.kraai", "/home/.config/kraai/providers.toml"]),
        grants(&[
            ("/home", true),
            ("/home/.kraai/script-output/session", false),
            ("/home/.kraai/eval/workspace", true),
        ]),
    );
    assert_eq!(
        denials.roots(),
        paths(&["/home/.kraai", "/home/.config/kraai/providers.toml"])
    );
    for (path, entry) in [
        ("/home/.kraai", Some(Deny::All)),
        ("/home/.kraai/sessions.db", Some(Deny::All)),
        ("/home/.kraai/script-output", Some(Deny::All)),
        ("/home/.kraai/script-output/other", Some(Deny::All)),
        ("/home/.kraai/script-output/session", Some(Deny::Write)),
        (
            "/home/.kraai/script-output/session/stdout",
            Some(Deny::Write),
        ),
        ("/home/.kraai/eval/workspace", None),
        ("/home/.kraai/eval/workspace/src", None),
        ("/home/.config/kraai/providers.toml", Some(Deny::All)),
    ] {
        assert_eq!(denials.entry(Path::new(path)), entry, "{path}");
    }
    assert!(denials.prunes(Path::new("/home/.kraai/eval/workspace")));
    assert!(!denials.prunes(Path::new("/home/.kraai/script-output/session")));
    assert!(!denials.prunes(Path::new("/home/.kraai/eval")));
}

#[test]
fn the_deepest_denied_path_wins_and_nested_ones_share_a_walk() {
    let denials = Denials::new(
        paths(&[
            "/home/.kraai",
            "/home/.kraai/sessions",
            "/home/.kraai/eval/workspace/.kraai-secret",
        ]),
        grants(&[("/home", false), ("/home/.kraai/eval/workspace", true)]),
    );
    assert_eq!(denials.roots(), paths(&["/home/.kraai"]));
    assert_eq!(
        denials.entry(Path::new("/home/.kraai/eval/workspace/.kraai-secret/key")),
        Some(Deny::All)
    );
    assert_eq!(
        denials.entry(Path::new("/home/.kraai/eval/workspace/file")),
        None
    );
    assert!(!denials.prunes(Path::new("/home/.kraai/eval/workspace")));
    assert!(denials.prunes(Path::new("/home/.kraai/eval/workspace/src")));
}

#[test]
fn a_grant_equal_to_a_denied_path_is_hidden() {
    let denials = Denials::new(paths(&["/home/.kraai"]), grants(&[("/home/.kraai", true)]));
    assert_eq!(denials.roots(), paths(&["/home/.kraai"]));
    assert_eq!(
        denials.entry(Path::new("/home/.kraai/sessions.db")),
        Some(Deny::All)
    );
}

#[test]
fn writable_grants_beat_read_only_grants_around_or_inside_them() {
    let denials = Denials::new(
        paths(&["/home/.kraai"]),
        grants(&[
            ("/home", true),
            ("/home/.kraai/read", false),
            ("/home/.kraai/read/write", true),
            ("/home/.kraai/write", true),
            ("/home/.kraai/write/read", false),
        ]),
    );
    for (path, entry) in [
        ("/home/.kraai/read/file", Some(Deny::Write)),
        ("/home/.kraai/read/write/file", None),
        ("/home/.kraai/write/read/file", None),
    ] {
        assert_eq!(denials.entry(Path::new(path)), entry, "{path}");
    }
}
