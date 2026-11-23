use kraai_execution_types::ScriptRewrites;
use kraai_provider_core::ScriptToolDefinition;
use kraai_script_protocol::{ScriptBlock, merge_scripts, parse_script_input};
use kraai_types::ScriptCall;

pub(super) fn merge_response_calls(calls: Vec<ScriptCall>) -> Option<(ScriptCall, ScriptRewrites)> {
    let total = calls.len();
    let mut valid = Vec::new();
    let mut scripts = Vec::new();
    let mut first_invalid = None;
    for call in calls {
        match parse_call(&call) {
            Some(script) => {
                valid.push(call);
                scripts.push(script);
            }
            None => {
                first_invalid.get_or_insert(call);
            }
        }
    }
    let rewrites = ScriptRewrites {
        bash: Vec::new(),
        merged_calls: if valid.len() > 1 { valid.len() } else { 0 },
        dropped_calls: total.saturating_sub(valid.len().max(1)),
    };
    let mut valid = valid.into_iter();
    let mut call = valid.next().or(first_invalid)?;
    if rewrites.merged_calls > 0 {
        call.input = merge_scripts(&scripts);
    }
    Some((call, rewrites))
}

fn parse_call(call: &ScriptCall) -> Option<ScriptBlock> {
    if call.arguments_error.is_some() || call.name != ScriptToolDefinition::NAME {
        return None;
    }
    parse_script_input(&call.input).ok()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests merge known calls")]
mod tests {
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
        let (merged, rewrites) =
            merge_response_calls(vec![call("a", "ls"), call("b", "pwd")]).unwrap();
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
}
