use super::*;

#[test]
fn progress_line_contains_elapsed_run_coordinates_and_phase() {
    let snapshot = kraai_eval::ProgressSnapshot {
        task_id: String::from("plural-files"),
        harness_name: String::from("kraai"),
        runner_version: String::from("git:abc123"),
        model_label: Some(String::from("gpt-test")),
        attempt: 2,
        phase: String::from("running grader 1/3"),
    };
    assert_eq!(
        format_progress_line(Duration::from_secs(3_661), &snapshot),
        "[01:01:01] task=plural-files | attempt=2 | running grader 1/3 | harness=kraai@git:abc123 | model=gpt-test"
    );
}

#[test]
fn progress_fits_narrow_terminals_without_wrapping_or_control_characters() {
    let snapshot = kraai_eval::ProgressSnapshot {
        task_id: String::from("event-stream"),
        harness_name: String::from("kraai"),
        runner_version: format!("sha256:{}", "f".repeat(64)),
        model_label: Some(String::from("gpt-test")),
        attempt: 2,
        phase: String::from("running harness"),
    };
    let line = format_progress_line(Duration::from_secs(1), &snapshot);
    for columns in [80, 120] {
        let fitted = fit_progress_line(&line, columns - 1);
        assert!(fitted.width() < columns);
        assert!(fitted.contains("event-stream"));
        assert!(fitted.contains("attempt=2"));
        assert!(fitted.contains("running harness"));
    }
    for columns in 0..16 {
        for text in [
            "任务 🦀\n\u{1b} long line",
            "1\u{fe0f}\u{20e3}1\u{fe0f}\u{20e3} long line",
        ] {
            let fitted = fit_progress_line(text, columns);
            assert!(fitted.width() <= columns);
            assert!(!fitted.chars().any(char::is_control));
        }
    }
}
