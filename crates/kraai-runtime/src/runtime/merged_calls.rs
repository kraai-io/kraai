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
mod tests;
