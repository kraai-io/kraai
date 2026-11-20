use std::io;
use std::path::{Path, PathBuf};

use kraai_execution_types::{SandboxCapabilities, SandboxCapability};

use crate::error::SandboxError;

/// Workspace entries that later turns trust: `.kraai/agents.toml` decides the permissions of
/// later scripts, and `AGENTS.md` becomes model instructions.
const PROTECTED: [Protected; 2] = [
    Protected {
        name: ".kraai",
        file: Some("agents.toml"),
    },
    Protected {
        name: "AGENTS.md",
        file: None,
    },
];

struct Protected {
    name: &'static str,
    /// The file Kraai reads inside a protected directory. `None` means the entry is the file.
    file: Option<&'static str>,
}

/// Keeps sandboxed scripts from changing the workspace entries in [`PROTECTED`]. Mounts or
/// Seatbelt rules make the existing entries read-only; [`ConfigGuard::restore`] undoes what
/// they cannot block once the script is gone.
#[derive(Debug)]
pub(crate) struct ConfigGuard {
    entries: Vec<GuardedEntry>,
    #[cfg(unix)]
    read_only: Vec<PathBuf>,
}

#[derive(Debug)]
struct GuardedEntry {
    path: PathBuf,
    state: EntryState,
    /// The file Kraai reads and its contents before the script, restored on Windows, which
    /// has no read-only mounts.
    #[cfg(windows)]
    file: PathBuf,
    #[cfg(windows)]
    contents: Option<Vec<u8>>,
    #[cfg(windows)]
    directory: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum EntryState {
    Absent,
    Symlink(PathBuf),
    Present,
}

impl ConfigGuard {
    pub(crate) fn applies_to(capabilities: &SandboxCapabilities) -> bool {
        !capabilities.is_unsandboxed()
            && (capabilities.contains(SandboxCapability::WorkspaceWrite)
                || capabilities.contains(SandboxCapability::HostWrite))
    }

    pub(crate) fn capture(workspace: &Path) -> Self {
        let entries = PROTECTED
            .iter()
            .map(|protected| GuardedEntry::capture(workspace, protected))
            .collect();
        Self {
            entries,
            #[cfg(unix)]
            read_only: resolved_entries(workspace),
        }
    }

    #[cfg(any(target_os = "macos", all(test, unix)))]
    pub(crate) fn entries(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().map(|entry| entry.path.as_path())
    }

    /// Existing protected paths, resolved through symlinks.
    #[cfg(unix)]
    pub(crate) fn read_only_paths(&self) -> &[PathBuf] {
        &self.read_only
    }

    /// Runs after every process of the script has exited.
    pub(crate) fn restore(&self) -> Result<(), SandboxError> {
        for entry in &self.entries {
            entry.restore().map_err(|error| {
                SandboxError::SandboxUnavailable(format!(
                    "unable to undo a sandboxed change to '{}': {error}",
                    entry.path.display()
                ))
            })?;
        }
        Ok(())
    }
}

impl GuardedEntry {
    fn capture(workspace: &Path, protected: &Protected) -> Self {
        let path = workspace.join(protected.name);
        let state = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => EntryState::Absent,
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::read_link(&path).map_or(EntryState::Present, EntryState::Symlink)
            }
            _ => EntryState::Present,
        };
        #[cfg(windows)]
        let file = protected
            .file
            .map_or_else(|| path.clone(), |file| path.join(file));
        Self {
            #[cfg(windows)]
            contents: (state == EntryState::Present)
                .then(|| std::fs::read(&file).ok())
                .flatten(),
            #[cfg(windows)]
            file,
            #[cfg(windows)]
            directory: protected.file.is_some(),
            path,
            state,
        }
    }

    fn restore(&self) -> io::Result<()> {
        match &self.state {
            EntryState::Absent => {
                if std::fs::symlink_metadata(&self.path).is_ok() {
                    remove_entry(&self.path)?;
                }
            }
            EntryState::Symlink(target) => {
                if std::fs::read_link(&self.path).ok().as_ref() != Some(target) {
                    if std::fs::symlink_metadata(&self.path).is_ok() {
                        remove_entry(&self.path)?;
                    }
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(target, &self.path)?;
                    #[cfg(windows)]
                    if self.directory {
                        std::os::windows::fs::symlink_dir(target, &self.path)?;
                    } else {
                        std::os::windows::fs::symlink_file(target, &self.path)?;
                    }
                }
            }
            EntryState::Present => {}
        }
        #[cfg(windows)]
        if self.state == EntryState::Present {
            self.restore_contents()?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn restore_contents(&self) -> io::Result<()> {
        let current = match std::fs::read(&self.file) {
            Ok(current) => Some(current),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if current == self.contents {
            return Ok(());
        }
        match &self.contents {
            Some(contents) => {
                if let Some(parent) = self.file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&self.file, contents)
            }
            None => remove_entry(&self.file),
        }
    }
}

#[cfg(unix)]
fn resolved_entries(workspace: &Path) -> Vec<PathBuf> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut paths = Vec::new();
    for protected in &PROTECTED {
        let entry = workspace.join(protected.name);
        if protected.file.is_some()
            && let Ok(directory) = entry.canonicalize()
            && directory.is_dir()
            && !workspace.starts_with(&directory)
        {
            paths.push(directory);
        }
        let file = protected
            .file
            .map_or_else(|| entry.clone(), |file| entry.join(file));
        if let Ok(file) = file.canonicalize()
            && file.is_file()
            && !paths.iter().any(|path| file.starts_with(path))
        {
            paths.push(file);
        }
    }
    paths
}

fn remove_entry(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        return std::fs::remove_dir_all(path);
    }
    #[cfg(windows)]
    if metadata.file_type().is_symlink() && std::fs::remove_dir(path).is_ok() {
        return Ok(());
    }
    std::fs::remove_file(path)
}
