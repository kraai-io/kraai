use super::*;

pub(super) async fn assert_script(source: &str, stdout: &str) {
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
        "[2]: 4,6\n4\na;b\nc\nonce\npiped\nlast\n",
    )
    .await;
}

#[tokio::test]
async fn function_values_stay_local_and_inline_blocks_emit_statements() {
    assert_script(
        r#"
def answer [] { "hidden"; 42 }
let closure = { "hidden"; 7 }
answer
do $closure
for n in [1 2] { "loop"; print $n }
if true { "branch"; "selected" }
module helpers { export def value [] { "hidden"; "imported" } }
use helpers value
value
"#,
        "42\n7\nloop\n1\nloop\n2\nbranch\nselected\nimported\n",
    )
    .await;
}

#[tokio::test]
async fn redirects_and_ignored_results_stay_silent() {
    assert_script(
        r#"
"ignored" | ignore
try { error make {msg: 'expected failure'} } catch { ignore }
"saved" out> result.txt
let nothing = ("assigned" | str uppercase)
open --raw result.txt
"#,
        "saved\n",
    )
    .await;
}

#[tokio::test]
async fn structured_results_and_print_share_toon_formatting() {
    let source = r#"
{nested: {items: [{name: "a very long string that must never wrap", enabled: true}]}, empty: []}
print {nested: {items: [{name: "a very long string that must never wrap", enabled: true}]}, empty: []}
[]
["a\nb" null [1 2]]
1..3
"#;
    let record = "nested:\n  items[1]{name,enabled}:\n    a very long string that must never wrap,true\nempty[0]:\n";
    assert_script(
        source,
        &format!("{record}{record}[0]:\n[3]:\n  - \"a\\nb\"\n  - null\n  - [2]: 1,2\n[3]: 1,2,3\n"),
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
    assert_eq!(String::from_utf8_lossy(&result.stdout), "ab: 2c\nAB");
    assert_eq!(String::from_utf8_lossy(&result.stderr), "error[2]: 1,2\n");
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
    for (failure_at, expected) in [(1, ""), (2, "[1]: 1\n")] {
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
async fn printing_inside_lazy_streams_comes_before_the_finished_list() {
    for sink in ["", " | print"] {
        assert_script(&format!("[1 2] | each {{|n| print $n}}{sink}"), "1\n2\n").await;
        assert_script(
            &format!(r#"[1 2] | each {{|n| print $n; {{value: $n, nested: ["a" "b"]}}}}{sink}"#),
            "1\n2\n[2]:\n  - value: 1\n    nested[2]: a,b\n  - value: 2\n    nested[2]: a,b\n",
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
        "1\n2\n[2]{value}:\n  1\n  2\n[2]{value}:\n  3\n  4\n"
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
    assert!(stdout.starts_with("msg: caught failure\n"), "{stdout}");
    assert!(stdout.contains("\nraw: "), "{stdout}");
    assert!(stdout.ends_with("\nafter\n"));
    assert!(result.stderr.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn byte_results_end_on_their_own_line_unless_print_suppresses_it() {
    assert_script(
        r#"
^/bin/sh -c 'printf raw'
^/bin/sh -c 'printf captured; printf warning >&2; exit 7' | complete
^/bin/sh -c 'printf "ends\n"'
^/bin/sh -c 'printf kept' | print -n
0x[41 42]
"after"
"#,
        "raw\nstdout: captured\nstderr: warning\nexit_code: 7\nends\nkeptAB\nafter\n",
    )
    .await;
}

#[tokio::test]
async fn unfinished_lists_keep_their_rows_when_execution_is_cancelled() {
    let workspace = TestWorkspace::new();
    let source = r#"
print "started"
[1 2] | each {|n| if $n == 2 { print "waiting"; sleep 1hr }; {n: $n} }
"#;
    let mut execution = plan(source.as_bytes().to_vec(), &workspace);
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    execution.output_events = Some(sender);
    let cancellation = CancellationToken::new();
    let run = tokio::spawn(execute(execution, cancellation.clone()));
    let mut received = Vec::new();
    hang_guard(async {
        while let Some(event) = receiver.recv().await {
            received.extend_from_slice(&event.bytes);
            if received.ends_with(b"started\nwaiting\n") {
                break;
            }
        }
    })
    .await
    .expect("output printed by the second row should stream after the first row");
    cancellation.cancel();
    let result = run
        .await
        .expect("execution panicked")
        .expect("cancel execution");
    assert_eq!(result.output.termination, Termination::Cancelled);
    assert_eq!(
        String::from_utf8_lossy(&result.output.stdout),
        "started\nwaiting\n[1]{n}:\n  1\n"
    );
}

#[tokio::test]
async fn markers_do_not_count_toward_the_capture_limit() {
    let expected: String = (1..=60000).map(|n| format!("{n}\n")).collect();
    assert_script("1..60000 | each {|n| print $n }; null", &expected).await;
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
        "a[2]: 1,2\nb: 3\nready\n"
    );
    assert!(result.stderr.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn unhandled_external_failures_stop_execution_and_name_the_command() {
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
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("exited with code 7"),
            "{source}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn row_commands_split_external_text_into_lines() {
    assert_script(
        r#"
def text [] { ^/bin/sh -c 'printf "b\na\nc\n"' }
text | first 2
text | where $it != "a"
text | length
text | sort
text | each {|line| $line | str uppercase }
text | str join ","
^/bin/sh -c 'exit 0' | length
text | str trim
"#,
        "[2]: b,a\n[2]: b,c\n3\n[3]: a,b,c\n[3]: B,A,C\nb,a,c\n0\nb\na\nc\n",
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn row_commands_keep_binary_external_output_as_bytes() {
    assert_script(
        r#"
^/bin/sh -c 'printf "a\000b"' | length
^/bin/sh -c 'printf "\377\376"' | first 1 | describe
"#,
        "3\nbinary (stream)\n",
    )
    .await;
}

#[tokio::test]
async fn row_commands_still_reject_unsupported_values() {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(b"'before'; 'text' | sort; 'after'".to_vec(), &workspace),
        CancellationToken::new(),
    )
    .await
    .expect("execute unsupported input")
    .output;
    assert_eq!(result.termination, Termination::Exited { code: Some(1) });
    assert_eq!(String::from_utf8_lossy(&result.stdout), "before\n");
    assert!(String::from_utf8_lossy(&result.stderr).contains("only_supports_this_input_type"));
}

#[cfg(unix)]
#[tokio::test]
async fn loop_bodies_emit_each_statement_result() {
    assert_script(
        r#"
for n in [1 2] { $n * 10 }
mut i = 0
while $i < 2 { $i += 1; $"w($i)" }
loop { if $i >= 3 { break }; $i += 1; {i: $i} }
for n in [1 2 3] { if $n == 2 { continue }; $n }
let silent = (for n in [1] { "never" })
try { "tried"; error make {msg: "fixture"} } catch { "caught" }
match 1 { 1 => { "arm"; "matched" } }
for n in [1] { ^/bin/sh -c 'printf "external\n"'; ^/bin/sh -c 'exit 0' }
$env.LAST_EXIT_CODE
"#,
        "10\n20\nw1\nw2\ni: 3\n1\n3\ntried\ncaught\narm\nmatched\nexternal\n0\n",
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn failing_externals_inside_loops_stop_execution() {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(
            b"for n in [1 2] { $n; ^/bin/sh -c 'exit 7' }; 'after'".to_vec(),
            &workspace,
        ),
        CancellationToken::new(),
    )
    .await
    .expect("execute failing loop")
    .output;
    assert_eq!(String::from_utf8_lossy(&result.stdout), "1\n");
    assert_ne!(result.termination, Termination::Exited { code: Some(0) });
}

#[tokio::test]
async fn output_has_no_colors_and_errors_use_compact_snippets() {
    let workspace = TestWorkspace::new();
    let result = execute(
        plan(
            b"[apple banana] | find ban\n[1 2] | get 5".to_vec(),
            &workspace,
        ),
        CancellationToken::new(),
    )
    .await
    .expect("execute plain output fixture")
    .output;
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(stdout, "[1]: banana\n");
    assert_eq!(
        stderr,
        "Error: Row number too large (max: 1).\nline 2: [1 2] | get 5\n  at column 13: index too large (max: 1)\ncode: nu::shell::access_beyond_end\n"
    );
}

#[tokio::test]
async fn warnings_and_errors_in_other_files_use_the_compact_format() {
    let workspace = TestWorkspace::new();
    std::fs::write(
        workspace.0.join("helper.nu"),
        "export def boom [] {\n    [1 2] | get 5\n}\n",
    )
    .expect("write helper module");
    let result = execute(
        plan(
            b"@deprecated \"use new\"\ndef old [] { 1 }\nold\nuse helper.nu\nhelper boom".to_vec(),
            &workspace,
        ),
        CancellationToken::new(),
    )
    .await
    .expect("execute diagnostics fixture")
    .output;
    assert_eq!(String::from_utf8_lossy(&result.stdout), "1\n");
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        "Warning: Command deprecated.\nline 3: old\n  at columns 1 to 3: old is deprecated and will be removed in a future release.\nhelp: use new\ncode: nu::parser::deprecated\nError: Row number too large (max: 1).\nline 2 of helper.nu:     [1 2] | get 5\n  at column 17: index too large (max: 1)\ncode: nu::shell::access_beyond_end\n"
    );
}
