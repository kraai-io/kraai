use std::path::Path;

use crate::WorkspaceFsError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTextEdit {
    pub start_line: u32,
    pub end_line: u32,
    pub old_text: String,
    pub new_text: String,
}

pub fn apply_exact_edits(
    path: &Path,
    contents: &str,
    edits: &[ExactTextEdit],
) -> Result<String, WorkspaceFsError> {
    if edits.is_empty() {
        return Ok(contents.to_owned());
    }
    let lines = index_lines(contents);
    let mut pending = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        pending.push(validate_edit(path, contents, &lines, index, edit)?);
    }

    pending.sort_by_key(|edit| (edit.start_line, edit.end_line));
    for window in pending.windows(2) {
        let [previous, current] = window else {
            continue;
        };
        if current.start_line <= previous.end_line {
            return Err(WorkspaceFsError::OverlappingEdits {
                path: path.to_path_buf(),
                first_start: previous.start_line,
                first_end: previous.end_line,
                second_start: current.start_line,
                second_end: current.end_line,
            });
        }
    }

    let output_len = pending.iter().fold(contents.len(), |length, edit| {
        length - (edit.end_byte - edit.start_byte) + edit.new_text.len()
    });
    let mut buffer = String::with_capacity(output_len);
    let mut remaining = contents;
    let mut consumed = 0;
    for edit in pending {
        let (unchanged, tail) = remaining.split_at(edit.start_byte - consumed);
        let (_, tail) = tail.split_at(edit.end_byte - edit.start_byte);
        buffer.push_str(unchanged);
        buffer.push_str(edit.new_text);
        remaining = tail;
        consumed = edit.end_byte;
    }
    buffer.push_str(remaining);
    Ok(buffer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineSpan {
    content_start: usize,
    content_end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingEdit<'a> {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    new_text: &'a str,
}

fn validate_edit<'a>(
    path: &Path,
    contents: &str,
    lines: &[LineSpan],
    index: usize,
    edit: &'a ExactTextEdit,
) -> Result<PendingEdit<'a>, WorkspaceFsError> {
    let edit_number = index.saturating_add(1);
    let start_line =
        usize::try_from(edit.start_line).map_err(|_error| WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        })?;
    let end_line =
        usize::try_from(edit.end_line).map_err(|_error| WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        })?;
    if start_line == 0 || end_line < start_line || end_line > lines.len() {
        return Err(WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        });
    }
    let first = lines.get(start_line.saturating_sub(1)).ok_or_else(|| {
        WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        }
    })?;
    let last = lines.get(end_line.saturating_sub(1)).ok_or_else(|| {
        WorkspaceFsError::InvalidLineRange {
            path: path.to_path_buf(),
            edit_number,
            start_line: edit.start_line,
            end_line: edit.end_line,
        }
    })?;
    let actual = contents
        .get(first.content_start..last.content_end)
        .ok_or_else(|| WorkspaceFsError::InvalidTextBoundary {
            path: path.to_path_buf(),
            edit_number,
        })?;
    if actual != edit.old_text {
        return Err(WorkspaceFsError::OldTextMismatch {
            path: path.to_path_buf(),
            edit_number,
            expected: edit.old_text.clone(),
            actual: actual.to_owned(),
        });
    }
    Ok(PendingEdit {
        start_line,
        end_line,
        start_byte: first.content_start,
        end_byte: last.content_end,
        new_text: &edit.new_text,
    })
}

fn index_lines(contents: &str) -> Vec<LineSpan> {
    if contents.is_empty() {
        return vec![LineSpan {
            content_start: 0,
            content_end: 0,
        }];
    }
    let bytes = contents.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let content_end = if index > start && bytes.get(index.saturating_sub(1)) == Some(&b'\r')
            {
                index.saturating_sub(1)
            } else {
                index
            };
            lines.push(LineSpan {
                content_start: start,
                content_end,
            });
            start = index.saturating_add(1);
        }
    }
    if start < bytes.len() {
        lines.push(LineSpan {
            content_start: start,
            content_end: bytes.len(),
        });
    }
    lines
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "edit tests directly assert validated text and errors"
)]
mod tests {
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
                    edit(4, 4, "delta", "last\nline"),
                    edit(1, 1, "alpha", "a"),
                    edit(2, 2, "beta", ""),
                ],
                "a\n\ngamma\nlast\nline\n",
            ),
            (
                "α\r\nβ\r\nkeep\r\n終",
                vec![edit(4, 4, "終", "🦀"), edit(1, 2, "α\r\nβ", "é\nnew")],
                "é\nnew\r\nkeep\r\n🦀",
            ),
            (
                "\n\nend",
                vec![edit(2, 2, "", "inserted")],
                "\ninserted\nend",
            ),
            ("", vec![edit(1, 1, "", "first\n")], "first\n"),
            ("unchanged\r\n", vec![], "unchanged\r\n"),
            ("remove", vec![edit(1, 1, "remove", "")], ""),
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
                edit(2, 3, "two\nthree", "last"),
                edit(1, 2, "one\ntwo", "first"),
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
                old_text: String::from("alpha"),
                new_text: String::from("one"),
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
}
