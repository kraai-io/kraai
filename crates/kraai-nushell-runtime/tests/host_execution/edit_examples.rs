use super::*;

#[tokio::test]
#[expect(clippy::panic_in_result_fn, reason = "test assertions report failures")]
async fn documented_edits_produce_the_shown_files()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixtures = [
        (
            "settings.conf",
            Some("enabled = false\nretries = 1\nobsolete = true\n"),
            "enabled = true\nverbose = false\nretries = 1\n",
        ),
        ("windows.conf", Some("count = 1\r\n"), "count = 2\r\n"),
        ("notes.txt", None, "ready\n"),
    ];
    assert_eq!(
        kraai_command_catalog::EDIT_FILE.examples.len(),
        fixtures.len()
    );
    for (example, (path, before, after)) in kraai_command_catalog::EDIT_FILE
        .examples
        .iter()
        .zip(fixtures)
    {
        let workspace = TestWorkspace::new();
        if let Some(before) = before {
            std::fs::write(workspace.0.join(path), before)?;
        }
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
        let result: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(
            result.get("operation"),
            Some(&serde_json::json!(if before.is_some() {
                "edited"
            } else {
                "created"
            }))
        );
    }
    Ok(())
}
