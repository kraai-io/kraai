use super::script_recovery::{completed_script_with_image, reopen_agent};
use color_eyre::eyre::{Result, eyre};
use kraai_nushell_runtime::ImageAttachmentHandler;
use kraai_types::{ContentPart, ConversationItem, MessageContent, ScriptExecutionId};

#[tokio::test]
async fn failed_script_images_recover_and_reopen_without_source_or_pins() -> Result<()> {
    let Some((harness, session_id, completed)) = completed_script_with_image(true).await? else {
        return Ok(());
    };
    let image = completed
        .record
        .images
        .values()
        .next()
        .ok_or_else(|| eyre!("missing image"))?
        .clone();
    assert!(!harness.data_dir.join("workspace/screenshot.png").exists());
    assert!(
        harness
            .runtime
            .context_state_store
            .list(&session_id)
            .await?
            .is_empty()
    );
    reopen_agent(&harness).await?;
    harness.runtime.recover_script_executions().await?;
    let history = harness.handle.get_chat_history(session_id.clone()).await?;
    let restored = history
        .get(&completed.record.result_message_id)
        .ok_or_else(|| eyre!("missing result"))?;
    let ConversationItem::ScriptResult { output, .. } = &restored.content else {
        return Err(eyre!("wrong result type"));
    };
    assert_eq!(output.images().collect::<Vec<_>>(), vec![&image]);
    assert!(!harness.runtime.image_store.read(&image).await?.is_empty());
    let record = &completed.record;
    let id = ScriptExecutionId::new(ulid::Ulid::generate());
    harness
        .runtime
        .execution_store
        .create(kraai_persistence::NewScriptExecution {
            id: id.clone(),
            session_id: session_id.clone(),
            source_message_id: record.source_message_id.clone(),
            call_id: record.call_id.clone(),
            profile: record.profile.clone(),
            source: Vec::new(),
            requested_capabilities: record.requested_capabilities.clone(),
            effective_capabilities: record.effective_capabilities.clone(),
            timeout: record.timeout,
        })
        .await?;
    harness.runtime.execution_store.mark_running(&id).await?;
    let mut handler = crate::runtime::images::DurableImageAttachments {
        execution_id: id,
        session_id: session_id.clone(),
        agent: harness.runtime.agent_manager.clone(),
        images: harness.runtime.image_store.clone(),
        executions: harness.runtime.execution_store.clone(),
    };
    assert_eq!(
        handler
            .attach_existing(1, image.id.clone())
            .await
            .map_err(|error| eyre!(error))?,
        image
    );
    handler.session_id = harness.handle.create_session().await?;
    assert!(handler.attach_existing(2, image.id.clone()).await.is_err());
    assert!(
        harness
            .runtime
            .context_state_store
            .list(&session_id)
            .await?
            .is_empty()
    );
    harness.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn image_only_queued_input_keeps_references_and_undo_restores_them() -> Result<()> {
    let Some(harness) = super::harness::RuntimeTestHarness::new(Vec::new()).await else {
        return Ok(());
    };
    let session_id =
        super::harness::create_session_with_profile(&harness.handle, "test-profile").await?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Png)?;
    let image = harness.handle.import_image(bytes.into_inner()).await?;
    let content = MessageContent(vec![ContentPart::Image { image }]);
    let preparation = harness
        .runtime
        .session_preparations
        .try_begin(&session_id)
        .ok_or_else(|| eyre!("missing preparation"))?;
    let result = harness
        .handle
        .send_content(
            session_id.clone(),
            content.clone(),
            "mock-model".into(),
            "mock".into(),
        )
        .await?;
    assert!(matches!(
        result,
        crate::SubmitMessageOutcome::Queued { position: 1 }
    ));
    let queued = harness.runtime.take_queued_messages(&session_id).await;
    assert_eq!(
        queued.first().map(|message| &message.message),
        Some(&content)
    );
    drop(preparation);
    let pending = harness
        .runtime
        .agent_manager
        .write()
        .await
        .prepare_start_stream(
            &session_id,
            content.clone(),
            kraai_types::ModelId::new("mock-model"),
            kraai_types::ProviderId::new("mock"),
        )
        .await?;
    {
        let mut manager = harness.runtime.agent_manager.write().await;
        manager.abort_streaming_message(&pending.message_id).await?;
        manager.clear_active_turn(&session_id);
    }
    assert_eq!(
        harness.handle.undo_last_user_message(session_id).await?,
        Some(content)
    );
    harness.shutdown().await;
    Ok(())
}
