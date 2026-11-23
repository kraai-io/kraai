use super::*;
use kraai_types::ToolCallId;

fn call(id: &str, input: &str) -> ScriptCall {
    ScriptCall {
        call_id: ToolCallId::new(id),
        name: ScriptToolDefinition::NAME.to_string(),
        input: input.to_string(),
        arguments_error: None,
        rewrite: None,
    }
}

#[test]
fn several_calls_become_the_first_call_running_every_script() {
    let (merged, rewrites) = merge_response_calls(vec![call("a", "ls"), call("b", "pwd")]).unwrap();
    assert_eq!(merged.call_id, ToolCallId::new("a"));
    assert_eq!(merged.input, "# timeout=1min\ndo {\nls\n}\n\ndo {\npwd\n}");
    assert_eq!((rewrites.merged_calls, rewrites.dropped_calls), (2, 0));
}

#[test]
fn invalid_calls_are_dropped_and_counted() {
    let mut broken = call("b", "{");
    broken.arguments_error = Some(String::from("bad json"));
    let (merged, rewrites) = merge_response_calls(vec![
        call("a", "# timeout=nope\nls"),
        broken,
        call("c", "pwd"),
    ])
    .unwrap();
    assert_eq!(merged.call_id, ToolCallId::new("c"));
    assert_eq!(merged.input, "pwd");
    assert_eq!((rewrites.merged_calls, rewrites.dropped_calls), (0, 2));

    let (merged, rewrites) =
        merge_response_calls(vec![call("a", "# timeout=nope\nls"), call("b", "")]).unwrap();
    assert_eq!(merged.call_id, ToolCallId::new("a"));
    assert_eq!((rewrites.merged_calls, rewrites.dropped_calls), (0, 1));
}

#[test]
fn a_single_call_is_unchanged() {
    let (merged, rewrites) = merge_response_calls(vec![call("a", "ls")]).unwrap();
    assert_eq!(merged.input, "ls");
    assert!(rewrites.is_empty());
    assert!(merge_response_calls(Vec::new()).is_none());
}
