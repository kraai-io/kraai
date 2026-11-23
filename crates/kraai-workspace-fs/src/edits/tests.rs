use super::*;

fn edit(start_line: u32, end_line: u32, old_text: &str, new_text: &str) -> ExactTextEdit {
    ExactTextEdit {
        start_line,
        end_line,
        old_text: old_text.to_owned(),
        new_text: new_text.to_owned(),
    }
}

#[test]
fn edits_use_original_ranges_and_preserve_untouched_text() {
    let cases = [
        (
            "alpha\nbeta\ngamma\ndelta\n",
            vec![
                edit(4, 4, "delta\n", "last\nline\n"),
                edit(1, 1, "alpha\n", "a\n"),
                edit(2, 2, "beta\n", ""),
            ],
            "a\ngamma\nlast\nline\n",
        ),
        (
            "α\r\nβ\r\nkeep\r\n終",
            vec![edit(4, 4, "終", "🦀"), edit(1, 2, "α\r\nβ\r\n", "é\nnew\n")],
            "é\nnew\nkeep\r\n🦀",
        ),
        (
            "\n\nend",
            vec![edit(2, 2, "\n", "inserted\n")],
            "\ninserted\nend",
        ),
        ("", vec![edit(1, 1, "", "first\n")], "first\n"),
        ("unchanged\r\n", vec![], "unchanged\r\n"),
        ("remove", vec![edit(1, 1, "remove", "")], ""),
        ("remove\r\n", vec![edit(1, 1, "remove\r\n", "")], ""),
        (
            "one\ntwo\n",
            vec![edit(1, 1, "one\n", "joined")],
            "joinedtwo\n",
        ),
        ("one\n", vec![edit(1, 1, "one\n", "one")], "one"),
        ("one", vec![edit(1, 1, "one", "one\r\n")], "one\r\n"),
    ];

    for (contents, edits, expected) in cases {
        assert_eq!(
            apply_exact_edits(Path::new("file"), contents, &edits).unwrap(),
            expected
        );
    }
}

#[test]
fn overlap_errors_use_sorted_original_line_ranges() {
    let error = apply_exact_edits(
        Path::new("file"),
        "one\ntwo\nthree\n",
        &[
            edit(2, 3, "two\nthree\n", "last\n"),
            edit(1, 2, "one\ntwo\n", "first\n"),
        ],
    )
    .unwrap_err();

    assert!(matches!(
        error,
        WorkspaceFsError::OverlappingEdits {
            first_start: 1,
            first_end: 2,
            second_start: 2,
            second_end: 3,
            ..
        }
    ));
}

#[test]
fn exact_edits_are_validated_before_any_replacement() {
    let edits = [
        ExactTextEdit {
            start_line: 1,
            end_line: 1,
            old_text: String::from("alpha\n"),
            new_text: String::from("one\n"),
        },
        ExactTextEdit {
            start_line: 2,
            end_line: 2,
            old_text: String::from("wrong"),
            new_text: String::from("two"),
        },
    ];
    let error = apply_exact_edits(Path::new("file"), "alpha\nbeta\n", &edits).unwrap_err();
    assert!(matches!(error, WorkspaceFsError::OldTextMismatch { .. }));
}

#[test]
fn matching_requires_exact_line_terminators() {
    for (contents, old_text) in [("one\n", "one"), ("one\r\n", "one\n"), ("one", "one\n")] {
        let error = apply_exact_edits(
            Path::new("file"),
            contents,
            &[edit(1, 1, old_text, "new\n")],
        )
        .unwrap_err();
        assert!(matches!(error, WorkspaceFsError::OldTextMismatch { .. }));
    }
}
