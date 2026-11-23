use std::fs;

use super::{capture_submission, init_repository};

#[test]
fn supplied_ignored_files_remain_tracked_when_submissions_are_replayed() {
    let root = std::env::temp_dir().join(format!(
        "kraai-eval-ignored-fixture-{}",
        ulid::Ulid::generate()
    ));
    let base = root.join("base");
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join(".gitignore"), "input.txt\ntarget/\n").unwrap();
    fs::write(base.join("input.txt"), "broken\n").unwrap();
    init_repository(&base).unwrap();
    let agent = root.join("agent");
    super::copy_tree(&base, &agent).unwrap();
    fs::write(agent.join("input.txt"), "fixed\n").unwrap();
    fs::create_dir(agent.join("target")).unwrap();
    fs::write(agent.join("target/output"), "build output\n").unwrap();
    let patch = root.join("submission.patch");
    capture_submission(&agent, &patch, 4096).unwrap();
    let grading = root.join("grading");
    super::replay_submission(&base, &grading, &patch).unwrap();
    assert!(
        fs::read_to_string(grading.join("input.txt")).unwrap() == "fixed\n",
        "ignored fixture edit was lost"
    );
    assert!(
        !grading.join("target/output").exists(),
        "ignored build output entered submission"
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn directory_sources_hash_executable_bits_and_copy_readonly_files_as_writable() {
    use std::os::unix::fs::PermissionsExt;
    let root =
        std::env::temp_dir().join(format!("kraai-eval-directory-{}", ulid::Ulid::generate()));
    fs::create_dir_all(root.join("fixture")).unwrap();
    let input = root.join("fixture/input");
    fs::write(&input, "input\n").unwrap();
    fs::set_permissions(&input, fs::Permissions::from_mode(0o444)).unwrap();
    let mut task: crate::TaskManifest = toml::from_str(
        "schema_version = 1\nid = 'directory'\nprompt = 'Repair input.'\n[source]\ndirectory = 'fixture'\n[[grader.commands]]\ncommand = ['true']\n",
    ).unwrap();
    task.validate(&root).unwrap();
    let initial = task.public_digest(&root).unwrap();
    let output = root.join("workspace");
    super::materialize_base(&task, &root, &output).unwrap();
    assert!(
        fs::metadata(output.join("input"))
            .unwrap()
            .permissions()
            .mode()
            & 0o200
            != 0
    );
    fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        initial == task.public_digest(&root).unwrap(),
        "readonly store permissions changed task identity"
    );
    fs::set_permissions(&input, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        initial != task.public_digest(&root).unwrap(),
        "executable permission did not change task identity"
    );
    fs::write(&input, "changed\n").unwrap();
    assert!(initial != task.public_digest(&root).unwrap());
    task.grader.hidden_patch = Some("fixture/input".into());
    assert!(
        task.validate(&root).is_err(),
        "hidden material was allowed inside the fixture"
    );
    task.grader.hidden_patch = None;
    std::os::unix::fs::symlink(&input, root.join("fixture/link")).unwrap();
    assert!(
        task.validate(&root).is_err(),
        "fixture symlink was accepted"
    );
    fs::remove_dir_all(root).unwrap();
}
