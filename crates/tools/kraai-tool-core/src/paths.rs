use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Read,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolFileOpenMode {
    Read,
    Edit,
    Create,
    Directory,
}

pub struct OpenedToolFile {
    path: PathBuf,
    file: File,
}

impl OpenedToolFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    #[cfg(target_os = "linux")]
    pub fn stable_path(&self) -> PathBuf {
        use std::os::fd::AsRawFd;

        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn stable_path(&self) -> PathBuf {
        self.path.clone()
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
    let path = normalize_tool_path(workspace_root, raw_path);
    if !path.exists() {
        return Err(format!("file does not exist: {}", path.display()));
    }
    if path.is_dir() {
        return Err(format!("path is a directory: {}", path.display()));
    }
    let mut opened = open_tool_file(workspace_root, raw_path, ToolFileOpenMode::Read)?;
    let mut contents = String::new();
    opened
        .file_mut()
        .read_to_string(&mut contents)
        .map_err(|error| format!("unable to read file {}: {}", opened.path().display(), error))?;

    Ok(TextFileRead {
        path: opened.path,
        contents,
    })
}

pub fn open_tool_file(
    workspace_root: &Path,
    raw_path: &str,
    mode: ToolFileOpenMode,
) -> Result<OpenedToolFile, String> {
    let path = normalize_tool_path(workspace_root, raw_path);
    let normalized_workspace = normalize_tool_path(workspace_root, ".");
    if path.starts_with(&normalized_workspace) {
        return open_workspace_file(&normalized_workspace, &path, mode);
    }

    let mut options = OpenOptions::new();
    match mode {
        ToolFileOpenMode::Read | ToolFileOpenMode::Directory => {
            options.read(true);
        }
        ToolFileOpenMode::Edit => {
            options.read(true).write(true);
        }
        ToolFileOpenMode::Create => {
            options.write(true).create_new(true);
        }
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("unable to open {}: {error}", path.display()))?;
    if mode == ToolFileOpenMode::Directory
        && !file
            .metadata()
            .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?
            .is_dir()
    {
        return Err(format!("path is not a directory: {}", path.display()));
    }
    Ok(OpenedToolFile { path, file })
}

#[cfg(target_os = "linux")]
fn open_workspace_file(
    workspace_root: &Path,
    path: &Path,
    mode: ToolFileOpenMode,
) -> Result<OpenedToolFile, String> {
    use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};

    let relative = path.strip_prefix(workspace_root).map_err(|error| {
        format!(
            "unable to resolve {} beneath workspace {}: {error}",
            path.display(),
            workspace_root.display()
        )
    })?;
    if relative.as_os_str().is_empty() && mode != ToolFileOpenMode::Directory {
        return Err(format!("path is a directory: {}", path.display()));
    }
    let workspace = File::open(workspace_root).map_err(|error| {
        format!(
            "unable to open workspace {}: {error}",
            workspace_root.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Ok(OpenedToolFile {
            path: path.to_path_buf(),
            file: workspace,
        });
    }
    let (flags, create_mode) = match mode {
        ToolFileOpenMode::Read => (OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()),
        ToolFileOpenMode::Edit => (OFlags::RDWR | OFlags::CLOEXEC, Mode::empty()),
        ToolFileOpenMode::Create => (
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH,
        ),
        ToolFileOpenMode::Directory => (
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ),
    };
    let fd = openat2(
        &workspace,
        relative,
        flags,
        create_mode,
        ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(|error| {
        format!(
            "unable to securely open workspace path {}: {error}",
            path.display()
        )
    })?;
    Ok(OpenedToolFile {
        path: path.to_path_buf(),
        file: File::from(fd),
    })
}

#[cfg(not(target_os = "linux"))]
fn open_workspace_file(
    _workspace_root: &Path,
    path: &Path,
    _mode: ToolFileOpenMode,
) -> Result<OpenedToolFile, String> {
    Err(format!(
        "secure descriptor-relative workspace access is unavailable on this platform: {}",
        path.display()
    ))
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
