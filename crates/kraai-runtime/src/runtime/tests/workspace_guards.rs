use color_eyre::eyre::Result;
use kraai_tool_edit_file::EditFileTool;
use kraai_tool_open_file::OpenFileTool;
use kraai_tool_read_file::ReadFileTool;

use super::harness::{RuntimeTestHarness, ScriptedChunk, create_session_with_profile};
use crate::Event;

#[tokio::test]
async fn batched_read_files_does_not_unlock_same_turn_edit_file() -> Result<()> {
    let Some(harness) = RuntimeTestHarness::new_with_tools(
        vec![
            vec![ScriptedChunk::plain(
                r#"<tool_call>
tool: read_files
files[1]: src/lib.rs
</tool_call>

<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,old,new
</tool_call>"#,
            )],
            vec![ScriptedChunk::plain("continuation complete")],
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
    let file_path = workspace_src.join("lib.rs");
    tokio::fs::write(&file_path, "old").await?;

    let session_id = create_session_with_profile(&harness.handle, "test-profile").await?;
    harness
        .handle
        .send_message(
            session_id.clone(),
            String::from("read then edit"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("batched read/edit tool results", |events| {
            let read_ready = events.iter().any(|event| {
                matches!(
                    event,
                    Event::ToolResultReady {
                        session_id: event_session,
                        tool_id,
                        success,
                        denied,
                        ..
                    } if event_session == &session_id
                        && tool_id == "read_files"
                        && *success
                        && !denied
                )
            });
            let edit_failed = events.iter().any(|event| {
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
                        && !*success
                        && !denied
                )
            });
            read_ready && edit_failed
        })
        .await;

    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    assert!(history.values().any(|message| {
        message
            .content
            .contains("edit_file requires the current file contents to be read first")
    }));
    assert_eq!(tokio::fs::read_to_string(&file_path).await?, "old");

    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn second_same_turn_edit_fails_after_first_changes_file() -> Result<()> {
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
  1,1,old,new
</tool_call>

<tool_call>
tool: edit_file
path: src/lib.rs
create: false
edits[1]{start_line,end_line,old_text,new_text}:
  1,1,new,newer
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
    tokio::fs::write(&file_path, "old").await?;

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
        .wait_for("open_file before double edit", |events| {
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
            String::from("double edit"),
            String::from("mock-model"),
            String::from("mock"),
        )
        .await?;

    harness
        .events
        .wait_for("double edit results", |events| {
            let successes = events
                .iter()
                .filter(|event| {
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
                .count();
            let failures = events
                .iter()
                .filter(|event| {
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
                            && !*success
                            && !denied
                    )
                })
                .count();
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
            successes >= 1 && failures >= 1 && stream_completions >= 4
        })
        .await;

    assert_eq!(tokio::fs::read_to_string(&file_path).await?, "new");

    harness.shutdown().await;
    Ok(())
}
