use super::*;

#[tokio::test]
async fn documented_edits_produce_the_shown_files() {
    let fixtures = [
        (
            "settings.conf",
            "enabled = false\nretries = 1\nobsolete = true\n",
            "enabled = true\nretries = 1\n",
            "edited",
        ),
        (
            "notes.md",
            "old draft\n",
            "# Notes\nfresh start\n",
            "replaced",
        ),
    ];
    assert_eq!(
        kraai_commands::edit_file::METADATA.examples.len(),
        fixtures.len()
    );
    for (example, (path, before, after, operation)) in kraai_commands::edit_file::METADATA
        .examples
        .iter()
        .zip(fixtures)
    {
        let workspace = TestWorkspace::new();
        std::fs::write(workspace.0.join(path), before).unwrap();
        let mut execution = plan(example.script_input.as_bytes().to_vec(), &workspace);
        execution
            .active_commands
            .push(String::from("kraai-edit-file"));
        let output = execute(execution, CancellationToken::new())
            .await
            .unwrap()
            .output;
        assert_eq!(
            output.termination,
            Termination::Exited { code: Some(0) },
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(workspace.0.join(path)).unwrap(),
            after.as_bytes()
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("success: true\n"), "{stdout}");
        assert!(
            stdout.contains(&format!("operation: {operation}\n")),
            "{stdout}"
        );
    }
}

#[tokio::test]
async fn overwrite_creates_missing_files_and_rejects_create() {
    let workspace = TestWorkspace::new();
    let mut execution = plan(
        b"kraai-edit-file new.txt --overwrite --contents 'first'\nkraai-edit-file new.txt --create --overwrite --contents 'second'".to_vec(),
        &workspace,
    );
    execution
        .active_commands
        .push(String::from("kraai-edit-file"));
    let output = execute(execution, CancellationToken::new())
        .await
        .unwrap()
        .output;
    assert_ne!(output.termination, Termination::Exited { code: Some(0) });
    assert_eq!(
        std::fs::read(workspace.0.join("new.txt")).unwrap(),
        b"first"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("operation: created\n"), "{stdout}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("use either --create or --overwrite"),
        "{stderr}"
    );
}
