use super::*;

#[test]
fn harness_dirs_keep_their_first_run_date() {
    let temporary =
        std::env::temp_dir().join(format!("kraai-eval-layout-{}", ulid::Ulid::generate()));
    let root = EvalRoot::open(&temporary).unwrap();
    let version = "482a30610bea240b574690a4dfdd23575eb5914a3fa9b913b263f6ee61bb747c";
    let model = model_dir("stealth/crouton-alpha", Some("high"));
    assert!(model == "stealth-crouton-alpha-high");
    let fresh = root
        .harness_dir("in-house", &model, "kraai", version)
        .unwrap();
    let today = chrono::Local::now().format(DATE).to_string();
    assert!(
        fresh.relative
            == Path::new("in-house/stealth-crouton-alpha-high")
                .join(format!("{today}-kraai-482a3061"))
    );
    let saved = root
        .path()
        .join("in-house/stealth-crouton-alpha-high/2026-01-02-kraai-482a3061");
    fs::create_dir_all(&saved).unwrap();
    fs::create_dir_all(
        root.path()
            .join("in-house/stealth-crouton-alpha-high/notes-kraai-482a3061"),
    )
    .unwrap();
    assert!(
        root.harness_dir("in-house", &model, "kraai", version)
            .unwrap()
            .path
            == saved
    );
    let other = root
        .harness_dir("in-house", &model, "codex", version)
        .unwrap();
    assert!(other.relative.ends_with(format!("{today}-codex-482a3061")));
    assert!(
        root.harness_dir("in-house", &model, "kraai", "short")
            .is_err()
    );
    assert!(
        root.lock_path(&fresh)
            == root
                .path()
                .join(".cache/locks/in-house/stealth-crouton-alpha-high")
                .join(format!("{today}-kraai-482a3061.lock"))
    );
    fs::remove_dir_all(temporary).unwrap();
}
