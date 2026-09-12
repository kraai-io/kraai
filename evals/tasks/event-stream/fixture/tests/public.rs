use event_stream::{Decoder, Event};

#[test]
fn complete_event() {
    assert_eq!(
        Decoder::new().feed(b"id: 7\nevent: update\ndata: hello\n\n"),
        Ok(vec![Event {
            event: "update".into(),
            id: Some("7".into()),
            data: "hello".into(),
        }])
    );
}

#[test]
fn chunked_event() {
    let mut decoder = Decoder::new();
    assert_eq!(decoder.feed(b"data: hel"), Ok(vec![]));
    assert_eq!(
        decoder.feed(b"lo\n\n"),
        Ok(vec![Event {
            event: "message".into(),
            id: None,
            data: "hello".into(),
        }])
    );
    assert_eq!(decoder.finish(), Ok(vec![]));
}
