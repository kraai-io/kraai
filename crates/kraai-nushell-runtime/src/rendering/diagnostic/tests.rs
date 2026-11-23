use miette::{LabeledSpan, SourceCode};

use super::{Diagnostic, Origin};

const SCRIPT: &str = "let x = [1 2]\n$x | get foo\nprint done";

#[derive(Debug)]
struct Fixture {
    labels: Vec<LabeledSpan>,
    cause: std::io::Error,
}

impl std::fmt::Display for Fixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cannot find column 'foo'")
    }
}

impl std::error::Error for Fixture {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

impl miette::Diagnostic for Fixture {
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new("try 'foo?'"))
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        Some(Box::new(self.labels.iter().cloned()))
    }
}

fn render(labels: Vec<LabeledSpan>) -> String {
    let source: &dyn SourceCode = &SCRIPT;
    Diagnostic::new(
        &Fixture {
            labels,
            cause: std::io::Error::other("inner failure"),
        },
        source,
        Some("nu::shell::column_not_found"),
        &Origin {
            script: "kraai-script.nu",
            cwd: None,
        },
    )
    .render()
}

#[test]
fn diagnostics_keep_messages_labelled_lines_help_and_codes() {
    assert_eq!(
        render(vec![
            LabeledSpan::new(Some("column 'foo' is missing".into()), 23, 3),
            LabeledSpan::new(Some("value originates here".into()), 8, 1),
        ]),
        "Error: Cannot find column 'foo'
caused by: inner failure
line 1: let x = [1 2]
  at column 9: value originates here
line 2: $x | get foo
  at columns 10 to 12: column 'foo' is missing
help: try 'foo?'
code: nu::shell::column_not_found"
    );
}

#[test]
fn multi_line_labels_mark_their_start_and_end() {
    assert_eq!(
        render(vec![LabeledSpan::new(Some("opened here".into()), 8, 15)]),
        "Error: Cannot find column 'foo'
caused by: inner failure
line 1: let x = [1 2]
  starting at column 9: opened here
line 2: $x | get foo
  ending at column 9
help: try 'foo?'
code: nu::shell::column_not_found"
    );
}
