use std::os::unix::fs::PermissionsExt;

use super::*;

#[test]
fn staging_collision_preserves_existing_directory() {
    let staging = std::env::temp_dir().join(format!(
        "kraai-eval-cargo-staging-collision-{}",
        ulid::Ulid::generate()
    ));
    fs::create_dir(&staging).unwrap();
    let sentinel = staging.join("foreign-data");
    fs::write(&sentinel, "preserve").unwrap();

    let error = create_staging_home(&staging).expect_err("reused an existing staging directory");
    assert!(error.kind() == std::io::ErrorKind::AlreadyExists);
    assert!(fs::read_to_string(&sentinel).unwrap() == "preserve");
    assert!(!staging.join("cargo-home").exists());

    fs::remove_dir_all(staging).unwrap();
}

#[test]
fn fetches_outside_task_with_isolated_cache_and_reuses_completed_bundle() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-cargo-dependencies-{}",
        ulid::Ulid::generate()
    ));
    let workspace = root.join("workspace");
    let cache = root.join("cache");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(workspace.join("Cargo.lock"), "version = 4\n").unwrap();
    fs::create_dir_all(workspace.join(".cargo")).unwrap();
    fs::write(
        workspace.join(".cargo/config.toml"),
        "[registry]\nglobal-credential-providers = [\"cargo:token-from-stdout credential-provider-sentinel\"]\n",
    ).unwrap();
    let shell = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("sh"))
                .find(|candidate| candidate.is_file())
        })
        .expect("test requires sh");
    let sentinel_marker = root.join("credential-provider-ran");
    let sentinel = workspace.join("credential-provider-sentinel");
    fs::write(
        &sentinel,
        format!(
            "#!{}\nset -eu\nprintf ran > {}\n",
            shell.display(),
            sentinel_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o755)).unwrap();
    let cargo = root.join("fake-cargo");
    fs::write(
        &cargo,
        format!(
            "#!{}\nset -eu\ntest \"$1\" = fetch\ntest \"$2\" = --locked\nprintf '%s' \"$PWD\" > \"$CARGO_HOME/fetch-cwd\"\nif [ -f .cargo/config.toml ]; then {} ; fi\nmkdir -p \"$CARGO_HOME/registry/index\" \"$CARGO_HOME/registry/cache\" \"$CARGO_HOME/git/db\"\nprintf fetched > \"$CARGO_HOME/fetched\"\n",
            shell.display(),
            sentinel.display()
        ),
    ).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let mkdir = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("mkdir"))
                .find(|candidate| candidate.is_file())
        })
        .expect("test requires mkdir");
    let rust = RustEnvironment {
        cargo: cargo.clone(),
        programs: vec![cargo, mkdir],
    };

    let first = prepare(&cache, &workspace, "task-digest", &rust).unwrap();
    assert!(!first.reused, "first fetch unexpectedly reused a cache");
    assert!(first.home.join("fetched").is_file());
    assert!(first.home.join("registry/cache").is_dir());
    assert!(first.home.join("git/db").is_dir());
    let fetch_cwd = PathBuf::from(fs::read_to_string(first.home.join("fetch-cwd")).unwrap());
    assert!(
        !fetch_cwd.starts_with(&workspace),
        "dependency fetch ran inside the task workspace"
    );
    assert!(
        !sentinel_marker.exists(),
        "task-local credential provider was executed"
    );

    let second = prepare(&cache, &workspace, "task-digest", &rust).unwrap();
    assert!(second.reused, "completed cache was not reused");
    assert!(second.key == first.key);
    assert!(second.home == first.home);

    fs::remove_dir_all(root).unwrap();
}
