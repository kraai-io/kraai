use color_eyre::eyre::Result;
use kraai_tool_edit_file::EditFileTool;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn native_toon_edit_file_call_executes_without_prior_read() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![
            vec![ScriptedChunk::plain(
                r#"<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,old,new
</tool_call>"#,
            )],
            vec![ScriptedChunk::plain("second continuation complete")],
        ],
        |tools| {
            tools.register_tool(EditFileTool);
        },
    )
    .await
    else {
        return Ok(());
    };

    let workspace_src = harness.data_dir.join("workspace").join("src");
    tokio::fs::create_dir_all(&workspace_src).await?;
    tokio::fs::write(workspace_src.join("lib.rs"), "old").await?;

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("edit file"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("native edit_file execution", |events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        tool_id,
                        success,
                        denied,
                        ..
                    } if event_session == &session_id
                        && tool_id == "edit_file"
                        && *success
                        && !denied
                )
            })
        })
        .await;

    harness
        .events
        .wait_for("edit_file continuation", |events| {
            let tool_result_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        tool_id,
                        success,
                        denied,
                        ..
                    } if event_session == &session_id
                        && tool_id == "edit_file"
                        && *success
                        && !denied
                )
            });
            let stream_completions = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::StreamComplete {
                            session_id: event_session,
                            ..
                        } if event_session == &session_id
                    )
                })
                .count();
            tool_result_ready && stream_completions >= 2
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(
        history
            .values()
            .any(|message| message.content == "second continuation complete")
    );
    assert_eq!(
        tokio::fs::read_to_string(workspace_src.join("lib.rs")).await?,
        "new"
    );

    harness.shutdown().await;
    Ok(())
}
