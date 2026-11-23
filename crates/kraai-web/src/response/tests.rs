use super::*;

#[test]
fn preserves_all_text_blocks_and_truncates_unicode() {
    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"🦀a"},{"type":"text","text":"bc"}]}}"#;
    let response = decode(body, 5).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(response.content, "🦀a\n\nb");
    assert!(response.truncated);
    let response = decode(body, 6).unwrap_or_else(|error| panic!("{error}"));
    assert!(!response.truncated);
}

#[test]
fn reads_sse_with_each_line_ending() {
    let body = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\ndata: \"result\":{\"content\":[{\"type\":\"text\",\"text\":\"found\"}]}}\n\n";
    for ending in ["\n", "\r\n", "\r"] {
        assert_eq!(
            decode(&body.replace('\n', ending), 100).map(|result| result.content),
            Ok(String::from("found")),
            "line ending: {ending:?}"
        );
    }
}

#[test]
fn reads_sse_with_a_leading_bom() {
    let body = "\u{feff}data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"found\"}]}}\n\n";
    for ending in ["\n", "\r\n", "\r"] {
        assert_eq!(
            decode(&body.replace('\n', ending), 100).map(|result| result.content),
            Ok(String::from("found")),
            "line ending: {ending:?}"
        );
    }
}

#[test]
fn errors_are_not_empty_results() {
    for body in [
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"failed"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[]}}"#,
        r#"{"jsonrpc":"2.0","id":2,"result":{"content":[]}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
        "data: not-json\n\n",
        "",
    ] {
        assert!(decode(body, 100).is_err(), "{body}");
    }
    assert!(decode(r#"{"jsonrpc":"2.0","id":1,"result":{"content":[]}}"#, 100).is_ok());
}
