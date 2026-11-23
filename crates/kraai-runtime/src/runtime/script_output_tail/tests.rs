use super::*;

fn texts(tail: &OutputTail) -> Vec<(ScriptOutputStream, String)> {
    tail.lines()
        .into_iter()
        .map(|line| (line.stream, line.text))
        .collect()
}

use ScriptOutputStream::{Stderr, Stdout};

#[test]
fn keeps_the_last_lines_of_both_streams_in_the_order_they_ended() {
    let mut tail = OutputTail::default();
    tail.push(Stdout, b"one\ntw");
    tail.push(Stderr, b"warn\n");
    tail.push(Stdout, b"o\nthree");
    assert_eq!(
        texts(&tail),
        [
            (Stdout, String::from("one")),
            (Stderr, String::from("warn")),
            (Stdout, String::from("two")),
            (Stdout, String::from("three")),
        ]
    );

    for index in 0..20 {
        tail.push(Stdout, format!("line {index}\n").as_bytes());
    }
    let lines = texts(&tail);
    assert_eq!(lines.len(), LINES);
    assert_eq!(lines.last(), Some(&(Stdout, String::from("line 19"))));
    assert_eq!(lines.first(), Some(&(Stdout, String::from("line 12"))));
}

#[test]
fn blank_lines_are_skipped_and_line_endings_are_trimmed() {
    let mut tail = OutputTail::default();
    tail.push(Stdout, b"\n\n  \nfirst  \r\nsecond\r\n\n");
    assert_eq!(
        texts(&tail),
        [
            (Stdout, String::from("first")),
            (Stdout, String::from("second"))
        ]
    );
}

#[test]
fn a_carriage_return_starts_the_line_over() {
    let mut tail = OutputTail::default();
    tail.push(Stderr, b"Building 1/3\rBuilding 2/3\r");
    assert_eq!(texts(&tail), [(Stderr, String::from("Building 2/3"))]);
    tail.push(Stderr, b"Building 3/3\ndone\n");
    assert_eq!(
        texts(&tail),
        [
            (Stderr, String::from("Building 3/3")),
            (Stderr, String::from("done")),
        ]
    );
}

#[test]
fn long_lines_keep_their_start_and_never_end_in_half_a_character() {
    let mut tail = OutputTail::default();
    let mut long = vec![b'a'; LINE_BYTES - 1];
    long.extend("é and more".as_bytes());
    long.push(b'\n');
    tail.push(Stdout, &long);
    let lines = texts(&tail);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines.first().map(|(_, text)| text.clone()),
        Some("a".repeat(LINE_BYTES - 1))
    );
}

#[test]
fn a_character_split_across_chunks_is_joined() {
    let mut tail = OutputTail::default();
    let (head, rest) = "héllo\n".as_bytes().split_at(2);
    tail.push(Stdout, head);
    tail.push(Stdout, rest);
    assert_eq!(texts(&tail), [(Stdout, String::from("héllo"))]);
}
