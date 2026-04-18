use color_eyre::eyre::Result;
use kraai_tool_edit_file::EditFileTool;
use kraai_tool_open_file::OpenFileTool;
use kraai_tool_read_file::ReadFileTool;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn native_toon_edit_file_call_executes_automatically_in_workspace() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![
            vec![ScriptedChunk::plain(
                r#"<tool_call>
tool: read_files
files[1]: src/lib.rs
</tool_call>"#,
            )],
            vec![ScriptedChunk::plain("first continuation complete")],
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
            tools.register_tool(ReadFileTool);
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
            String::from("read file"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("read_files continuation", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::StreamComplete { session_id: event_session, .. }
                            if event_session == &session_id
                    )
                })
                .count()
                >= 2
        })
        .await;

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("trigger edit"),
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
            tool_result_ready && stream_completions >= 4
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

#[tokio::test]
async fn open_file_refresh_allows_next_turn_edit_without_explicit_reread() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![
            vec![ScriptedChunk::plain(
                r#"<tool_call>
tool: open_file
path: src/lib.rs
</tool_call>"#,
            )],
            vec![ScriptedChunk::plain("first continuation complete")],
            vec![ScriptedChunk::plain(
                r#"<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,updated,rewritten
</tool_call>"#,
            )],
            vec![ScriptedChunk::plain("second continuation complete")],
        ],
        |tools| {
            tools.register_tool(OpenFileTool);
            tools.register_tool(EditFileTool);
        },
    )
    .await
    else {
        return Ok(());
    };

    let workspace_src = harness.data_dir.join("workspace").join("src");
    tokio::fs::create_dir_all(&workspace_src).await?;
    let file_path = workspace_src.join("lib.rs");
    tokio::fs::write(&file_path, "initial").await?;

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("open the file"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("open_file continuation", |events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        Event::StreamComplete { session_id: event_session, .. }
                            if event_session == &session_id
                    )
                })
                .count()
                >= 2
        })
        .await;

    tokio::fs::write(&file_path, "updated").await?;

    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("edit the open file"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("second turn edit succeeds", |events| {
            let edit_succeeded = events.iter().any(|event| {
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
                        Event::StreamComplete { session_id: event_session, .. }
                            if event_session == &session_id
                    )
                })
                .count();
            edit_succeeded && stream_completions >= 4
        })
        .await;

    assert_eq!(tokio::fs::read_to_string(&file_path).await?, "rewritten");

    harness.shutdown().await;
    Ok(())
}
