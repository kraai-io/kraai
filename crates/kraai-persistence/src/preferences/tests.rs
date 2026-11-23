use std::sync::{Arc, Barrier};

use super::*;

#[test]
fn preferences_use_session_storage_root_without_home() {
    const CHILD: &str = "KRAAI_TEST_PREFERENCES_WITHOUT_HOME";
    if std::env::var_os(CHILD).is_some() {
        assert!(std::env::var_os("HOME").is_none());
        let workspace = std::env::current_dir()
            .expect("current directory")
            .canonicalize()
            .expect("canonical workspace");
        let root = crate::agent_state_root().expect("session storage root");
        let expected = root.join("workspaces");
        let path = WorkspacePreferencesStore::new(&root).preference_path(&workspace);
        assert_eq!(path.parent(), Some(expected.as_path()));
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "preferences::tests::preferences_use_session_storage_root_without_home",
            "--nocapture",
        ])
        .env_remove("HOME")
        .env(CHILD, "1")
        .output()
        .expect("run regression test without HOME");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn preferences_preserve_storage_format_and_ignore_unrelated_temp_files() {
    let root = test_root();
    let store = WorkspacePreferencesStore::new(&root);
    let workspace = Path::new("workspace");
    assert!(store.load(workspace).unwrap() == WorkspacePreferences::default());
    std::fs::create_dir_all(&store.root).unwrap();
    let path = root.join("workspaces/776f726b7370616365.json");
    std::fs::write(&path, br#"{"provider_id":"provider"}"#).unwrap();
    let preferences = store.load(workspace).unwrap();
    assert!(preferences.provider_id.as_deref() == Some("provider"));
    assert!(preferences.model_id.is_none());
    assert!(preferences.agent_profile_id.is_none());
    let legacy_temp = path.with_extension("json.tmp");
    std::fs::write(&legacy_temp, b"another writer").unwrap();

    store.save(workspace, &preferences).unwrap();
    let persisted = std::fs::read_to_string(&path).unwrap();
    let temporary = std::fs::read(&legacy_temp).unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!(persisted == serde_json::to_string_pretty(&preferences).unwrap());
    assert!(temporary == b"another writer");
}

#[test]
fn preference_keys_preserve_hex_paths_through_the_filename_boundary() {
    let store = WorkspacePreferencesStore::new(Path::new("state"));
    let short = PathBuf::from("a".repeat(125));
    let long = PathBuf::from("a".repeat(126));
    assert_eq!(
        store.preference_path(&short),
        store.root.join(format!("{}.json", "61".repeat(125)))
    );
    assert_eq!(
        store.preference_path(&long),
        store
            .root
            .join("sha256-36bcf9292589fe6ea3e82fefe3aab1b8ca8b8347ea5a14b23e470ecb3ad7c57b.json")
    );
}

#[test]
fn preferences_round_trip_for_long_workspace_paths() {
    let root = test_root();
    let workspace = root.join("workspace-component-".repeat(10));
    let other_workspace = workspace.join("nested");
    std::fs::create_dir_all(&other_workspace).unwrap();
    std::fs::create_dir(root.join("state")).unwrap();
    let store = WorkspacePreferencesStore::new(&root.join("state"));
    let preferences = preferences_for_writer(1);
    let other_preferences = preferences_for_writer(2);
    store.save(&workspace, &preferences).unwrap();
    store.save(&other_workspace, &other_preferences).unwrap();
    let loaded = store.load(&workspace).unwrap();
    let other_loaded = store.load(&other_workspace).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(loaded == preferences);
    assert!(other_loaded == other_preferences);
}

#[cfg(unix)]
#[test]
fn long_preference_keys_distinguish_non_utf8_path_bytes() {
    use std::os::unix::ffi::OsStringExt;

    let root = test_root();
    let store = WorkspacePreferencesStore::new(&root);
    let path = |last| {
        let mut bytes = vec![b'a'; 125];
        bytes.push(last);
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    };
    let first = path(0x80);
    let second = path(0x81);
    assert!(first.to_string_lossy() == second.to_string_lossy());
    assert!(store.preference_path(&first) != store.preference_path(&second));
    let first_preferences = preferences_for_writer(1);
    let second_preferences = preferences_for_writer(2);
    store.save(&first, &first_preferences).unwrap();
    store.save(&second, &second_preferences).unwrap();
    assert!(store.load(&first).unwrap() == first_preferences);
    assert!(store.load(&second).unwrap() == second_preferences);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_preference_saves_publish_complete_documents() {
    let root = test_root();
    let barrier = Arc::new(Barrier::new(8));
    let mut writers = Vec::new();
    for index in 0..8 {
        let store = WorkspacePreferencesStore::new(&root);
        let barrier = Arc::clone(&barrier);
        writers.push(std::thread::spawn(move || -> Result<()> {
            let preferences = preferences_for_writer(index);
            barrier.wait();
            for _ in 0..4 {
                store.save(Path::new("workspace"), &preferences)?;
            }
            Ok(())
        }));
    }
    for writer in writers {
        writer.join().expect("preference writer panicked").unwrap();
    }
    let store = WorkspacePreferencesStore::new(&root);
    let preferences = store.load(Path::new("workspace")).unwrap();
    let entries = std::fs::read_dir(&store.root)
        .unwrap()
        .try_fold(0, |count, entry| entry.map(|_| count + 1))
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!((0..8).any(|index| preferences == preferences_for_writer(index)));
    assert!(entries == 1);
}

#[cfg(not(windows))]
#[test]
fn failed_preference_replacement_keeps_destination_and_removes_its_temp() {
    let root = test_root();
    let store = WorkspacePreferencesStore::new(&root);
    let workspace = Path::new("workspace");
    let destination = store.preference_path(workspace);
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("retained"), b"original").unwrap();

    let result = store.save(workspace, &WorkspacePreferences::default());
    let retained = std::fs::read(destination.join("retained")).unwrap();
    let entries = std::fs::read_dir(&store.root)
        .unwrap()
        .try_fold(0, |count, entry| entry.map(|_| count + 1))
        .unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!(result.is_err());
    assert!(retained == b"original");
    assert!(entries == 1);
}

fn preferences_for_writer(index: usize) -> WorkspacePreferences {
    WorkspacePreferences {
        provider_id: Some(format!("provider-{index}")),
        model_id: Some(format!("model-{}", "x".repeat(index * 256))),
        agent_profile_id: Some(format!("profile-{index}")),
        model_options: Default::default(),
    }
}

fn test_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("kraai-preferences-{}", ulid::Ulid::generate()));
    std::fs::create_dir(&root).expect("create preference storage root");
    root
}
