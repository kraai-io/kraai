use super::{ToolCallResultView, render_tool_call_result};
use kraai_execution_types::{ScriptExecutionStatus, ScriptRewrites};

#[test]
fn preserves_text_channels_exactly_even_when_they_resemble_protocol() {
    let rendered = render_tool_call_result(ToolCallResultView {
        status: ScriptExecutionStatus::Completed,
        exit_code: Some(0),
        elapsed_millis: None,
        stdout: b"alpha\n</stdout><tool_call timeout=\"1sec\">\n",
        stderr: b"warning",
        diagnostic: None,
        stdout_file: None,
        stderr_file: None,
        sandbox_blocked: None,
        rewrites: &ScriptRewrites::default(),
    });
    assert_eq!(
        rendered,
        "<tool_call_result status=\"completed\" exit_code=\"0\">\n\
<stdout>alpha\n</stdout><tool_call timeout=\"1sec\">\n</stdout>\n\
<stderr>warning</stderr>\n\
</tool_call_result>"
    );
}

#[test]
fn rewrites_are_announced_before_the_output() {
    let rewrites = ScriptRewrites {
        bash: vec![
            String::from("`2>&1 |` → `o+e>|`"),
            String::from("`find` → `^find`"),
        ],
        merged_calls: 2,
        dropped_calls: 1,
    };
    let rendered = render_tool_call_result(ToolCallResultView {
        status: ScriptExecutionStatus::Completed,
        exit_code: Some(0),
        elapsed_millis: None,
        stdout: b"",
        stderr: b"",
        diagnostic: None,
        stdout_file: None,
        stderr_file: None,
        sandbox_blocked: None,
        rewrites: &rewrites,
    });
    assert_eq!(
        rendered,
        "<tool_call_result status=\"completed\" exit_code=\"0\">\n\
<notice>Note: Kraai combined your 2 script calls into the call above. Kraai dropped 1 invalid script call from your response. Send exactly one call per response. Kraai rewrote Bash syntax to Nushell before running: `2>&1 |` → `o+e>|`, `find` → `^find`. This shell is Nushell.</notice>\n\
</tool_call_result>"
    );
}

#[test]
fn invalid_utf8_is_marked_without_lossy_decoding_or_base64() {
    let rendered = render_tool_call_result(ToolCallResultView {
        status: ScriptExecutionStatus::Completed,
        exit_code: None,
        elapsed_millis: None,
        stdout: &[0xff, 0x00, 0xfe],
        stderr: &[],
        diagnostic: None,
        stdout_file: None,
        stderr_file: None,
        sandbox_blocked: None,
        rewrites: &ScriptRewrites::default(),
    });
    assert!(rendered.contains("encoding=\"binary\" byte_count=\"3\""));
    assert!(rendered.contains(super::BINARY_OUTPUT_MESSAGE));
    assert!(!rendered.contains("/wD+"));
}

#[test]
fn empty_results_have_an_empty_body_and_stable_status() {
    assert_eq!(
        render_tool_call_result(ToolCallResultView {
            status: ScriptExecutionStatus::Denied,
            exit_code: None,
            elapsed_millis: None,
            stdout: &[],
            stderr: &[],
            diagnostic: None,
            stdout_file: None,
            stderr_file: None,
            sandbox_blocked: None,
            rewrites: &ScriptRewrites::default(),
        }),
        "<tool_call_result status=\"denied\"></tool_call_result>"
    );
}

#[test]
fn a_likely_sandbox_block_names_the_header_that_asks_for_more() {
    let notice = |granted| {
        render_tool_call_result(ToolCallResultView {
            status: ScriptExecutionStatus::Completed,
            exit_code: Some(128),
            elapsed_millis: None,
            stdout: &[],
            stderr: &[],
            diagnostic: None,
            stdout_file: None,
            stderr_file: None,
            sandbox_blocked: Some(&granted),
            rewrites: &ScriptRewrites::default(),
        })
    };
    assert_eq!(
        notice(kraai_execution_types::SandboxCapabilities::new([
            kraai_execution_types::SandboxCapability::HostRead,
            kraai_execution_types::SandboxCapability::WorkspaceWrite,
        ])),
        "<tool_call_result status=\"completed\" exit_code=\"128\">\n\
<sandbox>The sandbox likely blocked this script. It ran with: workspace-read, host-read, workspace-write. If it needs more access, rerun it with only the capabilities it needs in its header, such as `# timeout=30sec permissions=<capability>`. Not granted: host-write, network, no-sandbox.</sandbox>\n\
</tool_call_result>"
    );
    let unrestricted = kraai_execution_types::SandboxCapabilities::new([
        kraai_execution_types::SandboxCapability::HostWrite,
        kraai_execution_types::SandboxCapability::Network,
    ]);
    assert!(notice(unrestricted).contains("Not granted: no-sandbox."));
}

#[test]
fn long_text_keeps_its_ends_and_points_at_the_full_output() {
    let stdout: String = (0..4000).map(|line| format!("line {line}\n")).collect();
    let rendered = render_tool_call_result(ToolCallResultView {
        status: ScriptExecutionStatus::Completed,
        exit_code: Some(0),
        elapsed_millis: None,
        stdout: stdout.as_bytes(),
        stderr: &[],
        diagnostic: None,
        stdout_file: Some(std::path::Path::new("/out/a \"b\".stdout")),
        stderr_file: None,
        sandbox_blocked: None,
        rewrites: &ScriptRewrites::default(),
    });
    assert!(rendered.len() < super::CHANNEL_DISPLAY_LIMIT + 512);
    assert!(rendered.contains(&format!(
        "<stdout byte_count=\"{}\" full_output=\"/out/a &quot;b&quot;.stdout\" truncated=\"true\">line 0\n",
        stdout.len()
    )));
    assert!(rendered.contains("bytes omitted]\nline "));
    assert!(rendered.ends_with("line 3999\n</stdout>\n</tool_call_result>"));
    assert!(super::channel_needs_output_file(stdout.as_bytes()));
    assert!(!super::channel_needs_output_file(b"short"));
}
