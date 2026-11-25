#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use kraai_execution_types::SandboxHint;
use kraai_sandbox::{ExecutionOutput, Termination};

const WRITE_DENIALS: [&str; 4] = [
    "read-only file",
    "resource busy",
    "operation not permitted",
    "permission denied",
];
/// Lines a multi-line error may put between the failure and the path it names.
const NEARBY_LINES: usize = 2;
#[cfg(unix)]
const MAX_SOCKET_CANDIDATES: usize = 64;
#[cfg(unix)]
const AGENT_FAILURES: [&str; 2] = ["authentication agent", "connecting to agent"];

pub(crate) struct HintContext {
    pub(crate) workspace_roots: Vec<PathBuf>,
    #[cfg(unix)]
    pub(crate) agent_socket: Option<PathBuf>,
}

impl HintContext {
    /// Hints cover rules a sandboxed script cannot lift with more capabilities, so only a
    /// failure or an undone change gets one.
    pub(crate) fn hint(&self, output: &ExecutionOutput) -> Option<SandboxHint> {
        if output.protected_entries_undone {
            return Some(SandboxHint::ProtectedEntryUndone);
        }
        if !matches!(output.termination, Termination::Exited { code } if code != Some(0)) {
            return None;
        }
        let channels = [&output.stdout, &output.stderr].map(|bytes| String::from_utf8_lossy(bytes));
        if channels
            .iter()
            .any(|text| self.names_denied_protected_entry(text))
        {
            return Some(SandboxHint::ProtectedEntry);
        }
        #[cfg(unix)]
        if self.names_host_socket(&channels) {
            return Some(SandboxHint::HostSocket);
        }
        None
    }

    fn names_denied_protected_entry(&self, text: &str) -> bool {
        let lines: Vec<&str> = text.lines().collect();
        let denied: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                let line = line.to_lowercase();
                WRITE_DENIALS.iter().any(|denial| line.contains(denial))
            })
            .map(|(index, _)| index)
            .collect();
        denied.iter().any(|&index| {
            let start = index.saturating_sub(NEARBY_LINES);
            lines
                .iter()
                .skip(start)
                .take(index - start + NEARBY_LINES + 1)
                .any(|line| self.names_protected_entry(line))
        })
    }

    fn names_protected_entry(&self, line: &str) -> bool {
        !is_script_excerpt(line)
            && kraai_sandbox::protected_entry_names().any(|name| {
                line.match_indices(name).any(|(start, _)| {
                    ends_name(&line[start + name.len()..]) && self.in_workspace(&line[..start])
                })
            })
    }

    /// Whether a path ending at the end of `before` sits directly in the workspace.
    fn in_workspace(&self, before: &str) -> bool {
        let rooted = self.workspace_roots.iter().any(|root| {
            before
                .strip_suffix(['/', '\\'])
                .is_some_and(|parent| parent.ends_with(root.to_string_lossy().as_ref()))
        });
        let token = before
            .rfind(|character: char| character.is_whitespace() || "'\"`([<=,;".contains(character))
            .map_or(before, |end| &before[end + 1..]);
        rooted || matches!(token, "" | "./" | ".\\")
    }

    #[cfg(unix)]
    fn names_host_socket(&self, channels: &[std::borrow::Cow<'_, str>]) -> bool {
        let mut candidates = Vec::new();
        for path in channels.iter().flat_map(|text| absolute_paths(text)) {
            if candidates.len() == MAX_SOCKET_CANDIDATES {
                break;
            }
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
        if candidates.iter().any(|path| self.is_host_socket(path)) {
            return true;
        }
        self.agent_socket
            .as_deref()
            .is_some_and(|socket| self.is_host_socket(socket))
            && channels.iter().any(|text| {
                let text = text.to_lowercase();
                AGENT_FAILURES.iter().any(|failure| text.contains(failure))
            })
    }

    #[cfg(unix)]
    fn is_host_socket(&self, path: &Path) -> bool {
        use std::os::unix::fs::FileTypeExt;

        !self
            .workspace_roots
            .iter()
            .any(|root| path.starts_with(root))
            && std::fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
    }
}

/// Nushell diagnostics quote the failing source line, which names paths the script mentions
/// whether or not the failure involves them.
fn is_script_excerpt(line: &str) -> bool {
    line.strip_prefix("line ").is_some_and(|rest| {
        let rest = rest.trim_start_matches(|character: char| character.is_ascii_digit());
        rest.len() < line.len() - "line ".len()
            && (rest.starts_with(": ") || rest.starts_with(" of "))
    })
}

fn ends_name(after: &str) -> bool {
    let mut characters = after.chars();
    match characters.next() {
        None => true,
        Some('.') => !characters.next().is_some_and(char::is_alphanumeric),
        Some(character) => !(character.is_alphanumeric() || character == '_' || character == '-'),
    }
}

#[cfg(unix)]
fn absolute_paths(text: &str) -> impl Iterator<Item = &Path> {
    text.split(|character: char| character.is_whitespace() || "'\"`()[]<>=,;".contains(character))
        .filter_map(|token| {
            let token = token
                .strip_prefix("unix://")
                .or_else(|| token.strip_prefix("unix:"))
                .unwrap_or(token)
                .trim_end_matches([':', '.']);
            token.starts_with('/').then(|| Path::new(token))
        })
}

#[cfg(test)]
mod tests;
