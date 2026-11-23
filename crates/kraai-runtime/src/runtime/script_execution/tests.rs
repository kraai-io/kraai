use super::*;
use color_eyre::eyre::ensure;

#[tokio::test(start_paused = true)]
async fn output_reaches_clients_as_a_tail_at_most_ten_times_a_second() {
    let directory =
        std::env::temp_dir().join(format!("kraai-output-tail-{}", ulid::Ulid::generate()));
    let events = RuntimeEventSender::new(16);
    let mut received = events.subscribe();
    let (output, output_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(persist_output_events(
        Arc::new(kraai_persistence::SqliteScriptExecutionStore::new(
            &directory,
        )),
        ScriptExecutionId::new("execution"),
        output_rx,
        TailSink {
            events: events.clone(),
            session_id: SessionId::new("session"),
            call_id: String::from("call"),
        },
    ));
    let line = |text: &str| OutputEvent {
        stream: OutputStream::Stdout,
        bytes: format!("{text}\n").into_bytes(),
    };
    let mut next_tail = async || -> Result<(Vec<String>, Instant)> {
        let event = received.recv().await?.event;
        let crate::Event::ScriptOutputTail {
            session_id,
            call_id,
            lines,
        } = event
        else {
            return Err(eyre!("unexpected event {event:?}"));
        };
        ensure!(session_id.as_str() == "session" && call_id == "call");
        Ok((
            lines.into_iter().map(|line| line.text).collect(),
            Instant::now(),
        ))
    };

    let start = Instant::now();
    output.send(line("one")).unwrap();
    assert!(next_tail().await.unwrap() == (vec![String::from("one")], start));
    output.send(line("two")).unwrap();
    output.send(line("three")).unwrap();
    let (lines, at) = next_tail().await.unwrap();
    assert!(lines == ["one", "two", "three"], "{lines:?}");
    assert!(at == start + TAIL_INTERVAL);

    drop(output);
    let _ = task.await.unwrap();
    assert!(received.try_recv().is_err());
    let _ = std::fs::remove_dir_all(&directory);
}
