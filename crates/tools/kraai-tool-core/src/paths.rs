use std::{
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};

use kraai_types::{ExecutionPolicy, RiskLevel, ToolCallAssessment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToolPath {
    path: PathBuf,
    within_workspace: bool,
}

impl ResolvedToolPath {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_within_workspace(&self) -> bool {
        self.within_workspace
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFileRead {
    path: PathBuf,
    contents: String,
}

impl TextFileRead {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn contents(&self) -> &str {
        &self.contents
    }
}

pub fn normalize_tool_path(workspace_root: &Path, raw_path: &str) -> PathBuf {
    let path = Path::new(raw_path);
    let is_absolute = path.is_absolute();
    let base = if is_absolute {
        PathBuf::new()
    } else {
        workspace_root.to_path_buf()
    };

    let mut normalized = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized.parent().is_some() {
                    normalized.pop();
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    if is_absolute && !normalized.is_absolute() {
        normalized = Path::new("/").join(normalized);
    }

    normalized
}

pub fn resolve_tool_path(workspace_root: &Path, raw_path: &str) -> ResolvedToolPath {
    let path = normalize_tool_path(workspace_root, raw_path);
    let within_workspace = path_is_within_workspace(workspace_root, &path);
    ResolvedToolPath {
        path,
        within_workspace,
    }
}

/// Returns whether `candidate` resolves within `workspace_root`.
///
/// Existing symlinks are resolved before comparison so a path lexically inside
/// the workspace cannot escape through a symlink.
pub fn path_is_within_workspace(workspace_root: &Path, candidate: &Path) -> bool {
    canonicalize_for_workspace_check(workspace_root)
        .zip(canonicalize_for_workspace_check(candidate))
        .map(|(workspace_root, candidate)| candidate.starts_with(workspace_root))
        .unwrap_or_else(|| candidate.starts_with(workspace_root))
}

fn canonicalize_for_workspace_check(path: &Path) -> Option<PathBuf> {
    let mut missing_suffix = Vec::<OsString>::new();
    let mut cursor = path;

    loop {
        match fs::canonicalize(cursor) {
            Ok(mut canonical) => {
                for component in missing_suffix.iter().rev() {
                    canonical.push(component);
                }
                return Some(canonical);
            }
            Err(_) if cursor.exists() => return None,
            Err(_) => {
                let file_name = cursor.file_name()?.to_os_string();
                missing_suffix.push(file_name);
                cursor = cursor.parent()?;
            }
        }
    }
}

pub fn read_text_file(workspace_root: &Path, raw_path: &str) -> Result<TextFileRead, String> {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    read_text_path(resolved.path())
}

pub fn read_text_path(path: &Path) -> Result<TextFileRead, String> {
    if !path.exists() {
        return Err(format!("file does not exist: {}", path.display()));
    }
    if path.is_dir() {
        return Err(format!("path is a directory: {}", path.display()));
    }

    let contents = fs::read_to_string(path)
        .map_err(|error| format!("unable to read file {}: {}", path.display(), error))?;

    Ok(TextFileRead {
        path: path.to_path_buf(),
        contents,
    })
}

pub fn assess_read_path(
    workspace_root: &Path,
    raw_path: &str,
    workspace_reason_prefix: &str,
    outside_reason_prefix: &str,
) -> ToolCallAssessment {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    let reason = if resolved.is_within_workspace() {
        format!("{} {}", workspace_reason_prefix, resolved.path().display())
    } else {
        format!("{} {}", outside_reason_prefix, resolved.path().display())
    };

    ToolCallAssessment {
        risk: if resolved.is_within_workspace() {
            RiskLevel::ReadOnlyWorkspace
        } else {
            RiskLevel::ReadOnlyOutsideWorkspace
        },
        policy: ExecutionPolicy::AutonomousUpTo(RiskLevel::ReadOnlyWorkspace),
        reasons: vec![reason],
    }
}

pub fn assess_write_path(
    workspace_root: &Path,
    raw_path: &str,
    workspace_reason_prefix: &str,
    outside_reason_prefix: &str,
) -> ToolCallAssessment {
    let resolved = resolve_tool_path(workspace_root, raw_path);
    let reason = if resolved.is_within_workspace() {
        format!("{} {}", workspace_reason_prefix, resolved.path().display())
    } else {
        format!("{} {}", outside_reason_prefix, resolved.path().display())
    };

    ToolCallAssessment {
        risk: if resolved.is_within_workspace() {
            RiskLevel::UndoableWorkspaceWrite
        } else {
            RiskLevel::WriteOutsideWorkspace
        },
        policy: ExecutionPolicy::AlwaysAsk,
        reasons: vec![reason],
    }
}

pub fn format_text_with_line_numbers(contents: &str) -> String {
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}|{}", index + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}
