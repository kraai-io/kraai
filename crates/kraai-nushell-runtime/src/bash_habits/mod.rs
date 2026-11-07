mod heredoc;
mod rules;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex};

use nu_protocol::engine::{EngineState, StateWorkingSet};

use crate::StartupFile;

/// Each round re-parses the script, so later rounds only fix what earlier edits exposed.
const MAX_ROUNDS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRewrite {
    pub source: String,
    /// What changed, once each, such as `` `2>&1 |` → `o+e>|` ``.
    pub changes: Vec<String>,
}

/// Rewrites bash syntax that Nushell rejects or silently misreads, such as `2>/dev/null`,
/// `find . -name x`, or a heredoc, into the Nushell form with the same meaning. The script is
/// parsed with the same Kraai commands and startup files its shell will load, so user aliases
/// and definitions are respected. Returns `None` when nothing changed, the rewritten script
/// still fails to parse, or the startup files do not parse on their own.
pub fn rewrite_bash_habits(
    source: &str,
    active_commands: &[String],
    startup_files: &[StartupFile],
) -> Option<BashRewrite> {
    let engine = parse_engine(active_commands, startup_files)?;
    let edit_file = StateWorkingSet::new(&engine)
        .find_decl(kraai_commands::edit_file::METADATA.name.as_bytes())
        .is_some();
    let (mut current, mut changes) = heredoc::rewrite(source, edit_file)?;
    for _ in 0..MAX_ROUNDS {
        let edits = {
            let mut working_set = StateWorkingSet::new(&engine);
            let offset = working_set.next_span_start();
            nu_parser::parse(&mut working_set, None, current.as_bytes(), false);
            rules::edits(&working_set, &current, offset)
        };
        if edits.is_empty() {
            break;
        }
        current = rules::apply(&current, &edits);
        changes.extend(edits.into_iter().map(|edit| edit.change));
    }
    if current == source || !parses_cleanly(&engine, &current) {
        return None;
    }
    let mut unique = Vec::with_capacity(changes.len());
    for change in changes {
        if !unique.contains(&change) {
            unique.push(change);
        }
    }
    Some(BashRewrite {
        source: current,
        changes: unique,
    })
}

fn parses_cleanly(engine: &EngineState, source: &str) -> bool {
    let mut working_set = StateWorkingSet::new(engine);
    nu_parser::parse(&mut working_set, None, source.as_bytes(), false);
    working_set.parse_errors.is_empty() && working_set.compile_errors.is_empty()
}

#[derive(PartialEq, Eq)]
struct EngineKey {
    active_commands: Vec<String>,
    startup_files: Vec<StartupFile>,
}

/// The engine for the most recent shell setup, or `None` when its startup files do not parse.
/// Sessions usually share one profile, so a single entry is enough.
type CachedEngine = (EngineKey, Option<Arc<EngineState>>);

static ENGINE: Mutex<Option<CachedEngine>> = Mutex::new(None);

fn parse_engine(
    active_commands: &[String],
    startup_files: &[StartupFile],
) -> Option<Arc<EngineState>> {
    if let Ok(cached) = ENGINE.lock()
        && let Some((key, engine)) = cached.as_ref()
        && key.active_commands == active_commands
        && key.startup_files == startup_files
    {
        return engine.clone();
    }
    let engine = build_parse_engine(active_commands, startup_files).map(Arc::new);
    if let Ok(mut cached) = ENGINE.lock() {
        let key = EngineKey {
            active_commands: active_commands.to_vec(),
            startup_files: startup_files.to_vec(),
        };
        *cached = Some((key, engine.clone()));
    }
    engine
}

/// Mirrors the host's engine: Nushell's commands, then the active Kraai commands, then the
/// declarations from each startup file. Startup files are parsed, never evaluated.
fn build_parse_engine(
    active_commands: &[String],
    startup_files: &[StartupFile],
) -> Option<EngineState> {
    let mut engine = crate::host::default_engine();
    let registry =
        kraai_commands::command_registry(kraai_commands::CommandContext::parse_only()).ok()?;
    let mut working_set = StateWorkingSet::new(&engine);
    for command in registry.select(active_commands).ok()? {
        working_set.add_decl(command);
    }
    let delta = working_set.render();
    engine.merge_delta(delta).ok()?;
    for file in startup_files {
        let previous_file = engine.file.replace(file.path.clone());
        let delta = {
            let mut working_set = StateWorkingSet::new(&engine);
            let name = file.path.to_string_lossy();
            nu_parser::parse(&mut working_set, Some(&name), &file.contents, false);
            working_set
                .parse_errors
                .is_empty()
                .then(|| working_set.render())
        };
        engine.file = previous_file;
        engine.merge_delta(delta?).ok()?;
    }
    Some(engine)
}
