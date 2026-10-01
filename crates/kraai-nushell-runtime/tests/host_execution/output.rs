use super::*;

async fn assert_script(source: &str, stdout: &str) {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(source.as_bytes().to_vec(), &workspace),
        CancellationToken::new(),
    )
    .await
    .expect("execute output fixture")
    .output;
    assert_eq!(
        result.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), stdout);
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn each_outer_pipeline_emits_only_its_result() {
    assert_script(
        r#"
let input = [1 2 3]
$input | each { $in * 2 } | where $it > 2
mut counter = 3
$counter += 1
$counter
"a;b\nc"
print "once"
"piped" | print
"last"
"#,
        "4\n6\n4\na;b\nc\nonce\npiped\nlast\n",
    )
    .await;
}

#[tokio::test]
async fn nested_values_stay_local_and_definitions_share_state() {
    assert_script(
        r#"
def answer [] { "hidden"; 42 }
let closure = { "hidden"; 7 }
answer
do $closure
for n in [1 2] { "hidden"; print $n }
if true { "hidden"; "selected" }
module helpers { export def value [] { "hidden"; "imported" } }
use helpers value
value
"#,
        "42\n7\n1\n2\nselected\nimported\n",
    )
    .await;
}

#[tokio::test]
async fn redirects_and_ignored_results_stay_silent() {
    assert_script(
        r#"
"ignored" | ignore
"saved" out> result.txt
let nothing = ("assigned" | str uppercase)
open --raw result.txt
"#,
        "saved",
    )
    .await;
}

#[tokio::test]
async fn structured_results_and_print_share_nested_json_formatting() {
    let source = r#"
{nested: {items: [{name: "a very long string that must never wrap", enabled: true}]}, empty: []}
print {nested: {items: [{name: "a very long string that must never wrap", enabled: true}]}, empty: []}
[]
["a\nb" null [1 2]]
1..3
"#;
    let record = "{\"nested\":{\"items\":[{\"name\":\"a very long string that must never wrap\",\"enabled\":true}]},\"empty\":[]}\n";
    assert_script(
        source,
        &format!("{record}{record}[]\n[\"a\\nb\",null,[1,2]]\n[1,2,3]\n"),
    )
    .await;
}

#[tokio::test]
async fn print_preserves_flags_and_does_not_emit_twice() {
    let workspace = TestWorkspace::new();
    let source =
        r#"print -n "a"; print -n {b: 2}; print "c"; print -e {error: [1 2]}; print -r 0x[41 42]"#;
    let result = execute(
        plan(source.as_bytes().to_vec(), &workspace),
        CancellationToken::new(),
    )
    .await
    .expect("execute print fixture")
    .output;
    assert_eq!(result.termination, Termination::Exited { code: Some(0) });
    assert_eq!(String::from_utf8_lossy(&result.stdout), "a{\"b\":2}c\nAB");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "{\"error\":[1,2]}\n"
    );
}

#[tokio::test]
async fn parse_errors_prevent_all_execution_and_runtime_errors_stop_later_statements() {
    for (source, expected, code) in [
        ("'marker' | save marker.txt; let bad =", "", 1),
        (
            "'before'; error make {msg: 'fixture failure'}; 'after'",
            "before\n",
            1,
        ),
        ("'before'; exit 7; 'after'", "before\n", 7),
        (
            "'before'; return 'returned'; 'after'",
            "before\nreturned\n",
            0,
        ),
    ] {
        let workspace = TestWorkspace::new();
        let result = execute(
            plan(source.as_bytes().to_vec(), &workspace),
            CancellationToken::new(),
        )
        .await
        .expect("execute error fixture")
        .output;
        assert_eq!(
            result.termination,
            Termination::Exited { code: Some(code) },
            "{source}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            expected,
            "{source}"
        );
        assert!(!workspace.0.join("marker.txt").exists());
    }
}

#[tokio::test]
async fn lazy_stream_errors_preserve_emitted_values_and_remain_failures() {
    for (failure_at, expected) in [(1, ""), (2, "1\n")] {
        for sink in ["", " | print", " | print --stderr"] {
            let workspace = TestWorkspace::new();
            let source = format!(
                "[1 2] | each {{|n| if $n == {failure_at} {{ error make {{msg: 'stream failure'}} }}; $n }}{sink}; 'after'"
            );
            let result = execute(
                plan(source.into_bytes(), &workspace),
                CancellationToken::new(),
            )
            .await
            .expect("execute stream failure fixture")
            .output;
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert_eq!(result.termination, Termination::Exited { code: Some(1) });
            if sink.ends_with("--stderr") {
                assert!(stdout.is_empty());
                assert!(stderr.starts_with(expected), "{stderr}");
            } else {
                assert_eq!(stdout, expected);
            }
            assert!(stderr.contains("stream failure"));
        }
    }
}

#[tokio::test]
async fn printing_inside_lazy_streams_does_not_add_array_delimiters() {
    for sink in ["", " | print"] {
        assert_script(&format!("[1 2] | each {{|n| print $n}}{sink}"), "1\n2\n").await;
        assert_script(
            &format!(
                r#"[1 2] | each {{|n| print $n; {{value: $n, nested: ["a" "b"]}}}}{sink}"#
            ),
            "1\n{\"value\":1,\"nested\":[\"a\",\"b\"]}\n2\n{\"value\":2,\"nested\":[\"a\",\"b\"]}\n",
        )
        .await;
    }
}

#[tokio::test]
async fn lazy_streams_keep_print_output_on_its_requested_stream() {
    let workspace = TestWorkspace::new();
    let source = r#"
[1 2] | each {|n| print -e $n; {value: $n}} | print -e
[3 4] | each {|n| print $n; {value: $n}} | print -e
"#;
    let result = execute(
        plan(source.as_bytes().to_vec(), &workspace),
        CancellationToken::new(),
    )
    .await
    .expect("execute streamed print fixture")
    .output;
    assert_eq!(result.termination, Termination::Exited { code: Some(0) });
    assert_eq!(String::from_utf8_lossy(&result.stdout), "3\n4\n");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "1\n{\"value\":1}\n2\n{\"value\":2}\n{\"value\":3}\n{\"value\":4}\n"
    );
}

#[tokio::test]
async fn caught_errors_can_be_returned_as_structured_data() {
    let workspace = TestWorkspace::new();
    let source = r#"
try { error make {msg: 'caught failure'} } catch {|err| $err }
"after"
"#;
    let result = execute(
        plan(source.as_bytes().to_vec(), &workspace),
        CancellationToken::new(),
    )
    .await
    .expect("execute caught error")
    .output;
    assert_eq!(
        result.termination,
        Termination::Exited { code: Some(0) },
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).expect("text output");
    let record: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("error record"))
            .expect("JSON error record");
    assert!(
        record["msg"]
            .as_str()
            .is_some_and(|message| message.contains("caught failure"))
    );
    assert!(record["raw"].is_string());
    assert!(stdout.ends_with("\nafter\n"));
    assert!(result.stderr.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn external_text_and_complete_results_are_emitted_once() {
    assert_script(
        r#"
^/bin/sh -c 'printf raw'
^/bin/sh -c 'printf captured; printf warning >&2; exit 7' | complete
"after"
"#,
        "raw{\"stdout\":\"captured\",\"stderr\":\"warning\",\"exit_code\":7}\nafter\n",
    )
    .await;
}

#[tokio::test]
async fn results_stream_before_a_pipeline_finishes_and_cancellation_still_works() {
    let workspace = TestWorkspace::new();
    let source = r#"[1 2] | each {|n| if $n == 2 { sleep 30sec }; {n: $n} }"#;
    let mut execution = plan(source.as_bytes().to_vec(), &workspace);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    execution.output_events = Some(sender);
    let cancellation = CancellationToken::new();
    let run = tokio::spawn(execute(execution, cancellation.clone()));
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = receiver.recv().await {
            received.extend_from_slice(&event.bytes);
            if String::from_utf8_lossy(&received).contains("{\"n\":1}") {
                break;
            }
        }
    })
    .await
    .expect("first JSON element should stream before the second finishes");
    cancellation.cancel();
    let result = run
        .await
        .expect("execution panicked")
        .expect("cancel execution");
    assert!(String::from_utf8_lossy(&received).contains("{\"n\":1}"));
    assert_eq!(result.output.termination, Termination::Cancelled);
}

#[tokio::test]
async fn capture_limits_both_streams_and_keeps_draining_until_the_script_finishes() {
    let workspace = TestWorkspace::new();
    let source = r#"
1..200000 | each {|n| {n: $n, text: "automatic output"} }
print -e ("e" | fill -c e -w 1100000)
"finished" | save finished.txt
"#;
    let mut execution = plan(source.as_bytes().to_vec(), &workspace);
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    execution.output_events = Some(sender);
    let result = execute(execution, CancellationToken::new())
        .await
        .expect("execute large output")
        .output;
    assert_eq!(result.termination, Termination::Exited { code: Some(0) });
    assert_eq!(
        std::fs::read_to_string(workspace.0.join("finished.txt")).expect("read completion marker"),
        "finished"
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(event) = events.recv().await {
        match event.stream {
            kraai_sandbox::OutputStream::Stdout => stdout.extend(event.bytes),
            kraai_sandbox::OutputStream::Stderr => stderr.extend(event.bytes),
        }
    }
    assert_eq!(stdout, result.stdout);
    assert_eq!(stderr, result.stderr);
    for output in [&stdout, &stderr] {
        assert!(output.len() <= 1024 * 1024 + 150);
        assert!(String::from_utf8_lossy(output).contains("[kraai: output truncated after 1 MiB;"));
    }
}

#[tokio::test]
async fn inherited_display_hooks_do_not_reformat_model_output() {
    let workspace = TestWorkspace::new();
    let config_home = workspace.0.join("config");
    let config_dir = config_home.join("nushell");
    std::fs::create_dir_all(&config_dir).expect("create config directory");
    std::fs::write(
        config_dir.join("config.nu"),
        r#"
$env.config.hooks.display_output = { "hook output" }
"startup intermediate"
$env.STARTUP = "ready"
"#,
    )
    .expect("write config");
    let mut execution = plan(
        b"{a: [1 2]}; print {b: 3}; $env.STARTUP".to_vec(),
        &workspace,
    );
    execution.nushell_startup = NushellStartup::Inherit;
    execution
        .environment
        .insert("XDG_CONFIG_HOME".into(), config_home.display().to_string());
    let result = execute(execution, CancellationToken::new())
        .await
        .expect("execute inherited output")
        .output;
    assert_eq!(result.termination, Termination::Exited { code: Some(0) });
    assert_eq!(
        String::from_utf8_lossy(&result.stdout),
        "{\"a\":[1,2]}\n{\"b\":3}\nready\n"
    );
    assert!(result.stderr.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn unhandled_external_failures_stop_execution_but_complete_allows_inspection() {
    for source in [
        "^/bin/sh -c 'exit 7'; 'after'",
        "^/bin/sh -c 'exit 7' out> result.txt; 'after'",
    ] {
        let workspace = TestWorkspace::new();
        let result = execute(
            plan(source.as_bytes().to_vec(), &workspace),
            CancellationToken::new(),
        )
        .await
        .expect("execute external failure")
        .output;
        assert_eq!(
            result.termination,
            Termination::Exited { code: Some(7) },
            "{source}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&result.stdout).contains("after"));
    }
}
