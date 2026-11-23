use super::*;

#[test]
fn selection_rejects_duplicate_task_ids_and_accepts_paths_without_a_catalog() {
    let root = std::env::temp_dir().join(format!("kraai-eval-catalog-{}", ulid::Ulid::generate()));
    fs::create_dir_all(root.join("fixture")).unwrap();
    fs::write(root.join("fixture/input"), "input").unwrap();
    let path = root.join("task.toml");
    fs::write(
        &path,
        "schema_version = 1\nid = 'example'\nprompt = 'Repair the input.'\n[source]\ndirectory = 'fixture'\n[[grader.commands]]\ncommand = ['true']\n",
    ).unwrap();
    let selector = path.to_string_lossy().into_owned();
    let tasks = select(&root.join("missing"), std::slice::from_ref(&selector)).unwrap();
    assert!(tasks.len() == 1);
    assert!(select(&root, &[selector.clone(), selector]).is_err());
    fs::copy(&path, root.join("duplicate.toml")).unwrap();
    assert!(discover(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}
