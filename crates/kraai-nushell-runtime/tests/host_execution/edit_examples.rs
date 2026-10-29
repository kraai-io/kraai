use super::*;

#[tokio::test]
#[expect(clippy::panic_in_result_fn, reason = "test assertions report failures")]
async fn documented_edits_produce_the_shown_files()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixtures = [(
        "settings.conf",
        "enabled = false\nretries = 1\nobsolete = true\n",
        "enabled = true\nretries = 1\n",
    )];
    assert_eq!(
        kraai_commands::edit_file::METADATA.examples.len(),
        fixtures.len()
    );
    for (example, (path, before, after)) in kraai_commands::edit_file::METADATA
        .examples
        .iter()
        .zip(fixtures)
    {
        let workspace = TestWorkspace::new();
        std::fs::write(workspace.0.join(path), before)?;
        let mut execution = plan(example.script_input.as_bytes().to_vec(), &workspace);
        execution
            .active_commands
            .push(String::from("kraai-edit-file"));
        let output = execute(execution, CancellationToken::new()).await?.output;
        assert_eq!(
            output.termination,
            Termination::Exited { code: Some(0) },
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(workspace.0.join(path))?, after.as_bytes());
        let stdout = String::from_utf8(output.stdout)?;
        assert!(stdout.contains("success: true\n"), "{stdout}");
        assert!(stdout.contains("operation: edited\n"), "{stdout}");
    }
    Ok(())
}
