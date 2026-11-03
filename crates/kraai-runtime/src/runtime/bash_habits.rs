use kraai_execution_types::ScriptProfileSnapshot;
use kraai_script_protocol::parse_script_input;
use kraai_types::ScriptCall;

use super::script_environment::script_environment;

/// Rewrites bash syntax in a script call before anything records or runs it, so history, the
/// approval prompt, and the execution all use the corrected script and the model never sees its
/// mistake. The script is checked against the profile's commands and startup files, the same
/// shell it will run in. The model's version stays in `rewritten_from`.
pub(super) async fn rewrite_script_call(
    mut call: ScriptCall,
    profile: Option<ScriptProfileSnapshot>,
) -> ScriptCall {
    let Some(profile) = profile else {
        return call;
    };
    if call.arguments_error.is_some() {
        return call;
    }
    let Ok(script) = parse_script_input(&call.input) else {
        return call;
    };
    let Ok(source) = String::from_utf8(script.source) else {
        return call;
    };
    let Ok(environment) = script_environment(&profile) else {
        return call;
    };
    let header_len = call.input.len().saturating_sub(source.len());
    let rewritten = tokio::task::spawn_blocking(move || {
        let startup_files =
            kraai_nushell_runtime::startup_files(profile.nushell_startup, &environment).ok()?;
        kraai_nushell_runtime::rewrite_bash_habits(&source, &profile.commands, &startup_files)
    })
    .await;
    match rewritten {
        Ok(Some(source)) => {
            let header = call.input.get(..header_len).unwrap_or_default();
            let input = format!("{header}{source}");
            call.rewritten_from = Some(std::mem::replace(&mut call.input, input));
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(%error, "Rewriting bash syntax failed"),
    }
    call
}
