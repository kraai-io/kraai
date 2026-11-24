use super::*;

#[test]
fn line_numbering_preserves_blank_lines_line_endings_and_unicode() {
    for (input, expected) in [
        ("", ""),
        ("\n", "1|\n"),
        ("\n\n", "1|\n2|\n"),
        ("one\n", "1|one\n"),
        ("one\r\ntwo\n\n", "1|one\r\n2|two\n3|\n"),
        ("one\rtwo", "1|one\rtwo"),
        ("é\n模型\n🦀", "1|é\n2|模型\n3|🦀"),
    ] {
        let mut formatted = String::new();
        append_text_with_line_numbers(&mut formatted, input);
        assert_eq!(formatted, expected);
    }
}

#[test]
fn numbered_contents_can_be_used_for_exact_edits_without_losing_line_endings() {
    for contents in [
        "",
        "\n",
        "alpha\r\nbeta\r\n",
        "alpha\r\nbeta\nlast",
        "α\n終",
    ] {
        let mut numbered = String::new();
        append_text_with_line_numbers(&mut numbered, contents);
        let old_text: String = numbered
            .split_inclusive('\n')
            .filter_map(|line| line.split_once('|').map(|(_, text)| text))
            .collect();
        assert_eq!(old_text, contents);
        let edited = kraai_workspace_fs::apply_exact_edits(
            std::path::Path::new("fixture"),
            contents,
            &[kraai_workspace_fs::ExactTextEdit {
                start_line: 1,
                end_line: contents.split_inclusive('\n').count().max(1) as u32,
                old_text,
                new_text: String::from("replacement\r\n"),
            }],
        );
        assert_eq!(edited.ok().as_deref(), Some("replacement\r\n"));
    }
}

#[test]
fn state_folds_pin_reauthorization_and_unpin_in_order() {
    let path = PathBuf::from("/workspace/file.rs");
    let mut state = ContextState::default();
    state.apply(
        "opened",
        ContextStateMutation::PinFile {
            path: path.clone(),
            scope: PinnedFileScope::Workspace {
                root: PathBuf::from("/workspace"),
            },
        },
    );
    state.apply(
        "opened",
        ContextStateMutation::PinFile {
            path: path.clone(),
            scope: PinnedFileScope::Host,
        },
    );
    assert_eq!(state.opened_files.len(), 1);
    assert_eq!(
        state.opened_files.first().map(|file| &file.scope),
        Some(&PinnedFileScope::Host)
    );
    state.apply(
        "opened",
        ContextStateMutation::UnpinFile { path, reason: None },
    );
    assert!(state.opened_files.is_empty());
}
fn message(id: &str, content: kraai_types::ConversationItem) -> kraai_types::Message {
    kraai_types::Message {
        id: MessageId::new(id),
        parent_id: None,
        content,
        status: kraai_types::MessageStatus::Complete,
        agent_profile_id: None,
        generation: None,
    }
}

fn section(key: &'static str, text: &str) -> InstructionSection {
    InstructionSection {
        key,
        label: format!("{key} label"),
        text: text.into(),
    }
}

fn snapshot(key: &str, anchor: Option<&str>, text: &str) -> InstructionSnapshot {
    InstructionSnapshot {
        key: key.into(),
        label: format!("{key} label"),
        anchor: anchor.map(MessageId::new),
        text: text.into(),
    }
}

#[test]
fn instructions_stay_in_place_until_they_change_and_report_every_change() {
    let user = || kraai_types::ConversationItem::User {
        content: "request".into(),
    };
    let first_turn = [message("user", user())];
    let mut notifications = Vec::new();
    let refreshed = refresh_instructions(
        &[],
        vec![section("agents", "rules")],
        &MessageId::new("user"),
        &first_turn.iter().map(|message| &message.id).collect(),
        &first_turn,
        &mut notifications,
    );
    assert_eq!(refreshed, [snapshot("agents", None, "rules")]);
    assert!(notifications.is_empty());

    let history = [
        message("user", user()),
        message(
            "reply",
            kraai_types::ConversationItem::Assistant { items: Vec::new() },
        ),
        message("next", user()),
    ];
    let retained = history.iter().map(|message| &message.id).collect();
    let previous = [
        snapshot("kept", Some("reply"), "same"),
        snapshot("compacted", Some("gone"), "same"),
        snapshot("changed", Some("user"), "old"),
        snapshot("removed", None, "old"),
    ];
    let refreshed = refresh_instructions(
        &previous,
        vec![
            section("kept", "same"),
            section("compacted", "same"),
            section("changed", "new"),
            section("added", "new"),
        ],
        &MessageId::new("next"),
        &retained,
        &history,
        &mut notifications,
    );
    assert_eq!(
        refreshed,
        [
            snapshot("kept", Some("reply"), "same"),
            snapshot("compacted", None, "same"),
            snapshot("changed", Some("next"), "new"),
            snapshot("added", Some("next"), "new"),
        ]
    );
    assert_eq!(
        notifications,
        [
            "removed label was removed; its instructions no longer apply.",
            "changed label changed; the version below replaces the earlier one.",
            "added label was added.",
        ]
    );
}

#[test]
fn snapshot_paths_are_workspace_relative_and_drop_verbatim_prefixes() {
    let workspace = Path::new("workspace");
    let inside = workspace.join("src").join("lib.rs");
    assert_eq!(
        display_path(&inside, workspace),
        Path::new("src").join("lib.rs").display().to_string()
    );
    let outside = Path::new("elsewhere").join("notes.txt");
    assert_eq!(
        display_path(&outside, workspace),
        outside.display().to_string()
    );
    for (verbatim, plain) in [
        (r"\\?\C:\Users\notes.txt", r"C:\Users\notes.txt"),
        (
            r"\\?\UNC\server\share\notes.txt",
            r"\\server\share\notes.txt",
        ),
        (r"\\?\Volume{1}\notes.txt", r"\\?\Volume{1}\notes.txt"),
    ] {
        assert_eq!(
            without_verbatim_prefix(Path::new(verbatim)),
            Path::new(plain)
        );
    }
}

#[test]
fn rendered_snapshot_names_the_file_and_numbers_lines() {
    assert_eq!(
        render_file("src/lib.rs", "one\ntwo"),
        "Opened file: src/lib.rs\n```text\n1|one\n2|two\n```\nNo newline at end of file."
    );
}
