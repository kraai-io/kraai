use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

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

type Snapshot = Arc<Vec<GuardedEntry>>;

/// Runs that overlap in one workspace share the snapshot the first of them captured, so a
/// later run never records an earlier run's change as the original.
static ACTIVE: LazyLock<Mutex<HashMap<PathBuf, (usize, Snapshot)>>> = LazyLock::new(Mutex::default);

/// Keeps sandboxed scripts from changing the workspace entries in [`PROTECTED`]. Mounts or
/// Seatbelt rules make the existing entries read-only; [`ConfigGuard::restore`] undoes what
/// they cannot block once the script is gone.
#[derive(Debug)]
pub(crate) struct ConfigGuard {
    workspace: PathBuf,
    entries: Snapshot,
}

#[derive(Debug)]
struct GuardedEntry {
    path: PathBuf,
    state: EntryState,
    directory: bool,
    /// The file Kraai reads, restored on Windows, which has no read-only mounts.
    #[cfg(windows)]
    file: Option<FileSnapshot>,
}

#[cfg(windows)]
#[derive(Debug)]
struct FileSnapshot {
    path: PathBuf,
    state: EntryState,
    /// Where the path resolved, so restoring never writes through a link the script planted.
    location: PathBuf,
    contents: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryState {
    Absent,
    Symlink(PathBuf),
    Present { directory: bool },
}

impl ConfigGuard {
    pub(crate) fn applies_to(capabilities: &SandboxCapabilities) -> bool {
        !capabilities.is_unsandboxed()
            && (capabilities.contains(SandboxCapability::WorkspaceWrite)
                || capabilities.contains(SandboxCapability::HostWrite))
    }

    pub(crate) fn capture(workspace: &Path) -> Result<Self, SandboxError> {
        let workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        let entries = match active.get_mut(&workspace) {
            Some((runs, entries)) => {
                *runs += 1;
                Arc::clone(entries)
            }
            None => {
                let entries = Arc::new(
                    PROTECTED
                        .iter()
                        .map(|protected| GuardedEntry::capture(&workspace, protected))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                active.insert(workspace.clone(), (1, Arc::clone(&entries)));
                entries
            }
        };
        drop(active);
        Ok(Self { workspace, entries })
    }

    /// Runs after every process of the script has exited.
    pub(crate) fn restore(&self) -> Result<(), SandboxError> {
        for entry in self.entries.iter() {
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

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((runs, _)) = active.get_mut(&self.workspace) {
            *runs -= 1;
            if *runs == 0 {
                active.remove(&self.workspace);
            }
        }
    }
}

/// The protected workspace entries, whether or not they exist.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn entry_paths(workspace: &Path) -> impl Iterator<Item = PathBuf> {
    PROTECTED
        .iter()
        .map(move |protected| workspace.join(protected.name))
}

/// Existing protected paths, resolved through symlinks.
#[cfg(unix)]
pub(crate) fn read_only_paths(workspace: &Path) -> Vec<PathBuf> {
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

impl GuardedEntry {
    fn capture(workspace: &Path, protected: &Protected) -> Result<Self, SandboxError> {
        let path = workspace.join(protected.name);
        let state = entry_state(&path)?;
        let file = protected
            .file
            .map_or_else(|| path.clone(), |file| path.join(file));
        let file_state = if state == EntryState::Absent || file == path {
            None
        } else if cfg!(unix)
            && std::fs::symlink_metadata(&file)
                .is_err_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
        {
            // The script cannot open the directory either, and its read-only mount or Seatbelt
            // rule keeps the script from changing its permissions.
            None
        } else {
            Some(entry_state(&file)?)
        };
        #[cfg(unix)]
        if state != EntryState::Absent {
            reject_hard_links(&file)?;
        }
        #[cfg(windows)]
        let file = match file_state {
            None if state == EntryState::Absent => None,
            file_state => Some(FileSnapshot::capture(
                file,
                file_state.unwrap_or_else(|| state.clone()),
            )?),
        };
        #[cfg(not(windows))]
        let _ = file_state;
        Ok(Self {
            directory: protected.file.is_some(),
            path,
            state,
            #[cfg(windows)]
            file,
        })
    }

    fn restore(&self) -> io::Result<()> {
        restore_state(&self.path, &self.state, self.directory)?;
        #[cfg(windows)]
        if let Some(file) = &self.file {
            file.restore()?;
        }
        Ok(())
    }
}

#[cfg(windows)]
impl FileSnapshot {
    fn capture(path: PathBuf, state: EntryState) -> Result<Self, SandboxError> {
        let unreadable = |error: io::Error| {
            SandboxError::SandboxUnavailable(format!(
                "unable to read '{}' before running a sandboxed script: {error}",
                path.display()
            ))
        };
        let location = location(&path).map_err(unreadable)?;
        let contents = read_optional(&path).map_err(unreadable)?;
        Ok(Self {
            path,
            state,
            location,
            contents,
        })
    }

    fn restore(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        restore_state(&self.path, &self.state, false)?;
        let Some(contents) = &self.contents else {
            return Ok(());
        };
        let location = location(&self.path)?;
        if location != self.location {
            return Err(io::Error::other(format!(
                "it now resolves to '{}' instead of '{}'",
                location.display(),
                self.location.display()
            )));
        }
        if read_optional(&self.path)?.as_ref() != Some(contents) {
            std::fs::write(&self.path, contents)?;
        }
        Ok(())
    }
}

/// Reads an entry's state. Links must resolve, because a script could create a missing
/// target and the unchanged link would then trust it.
fn entry_state(path: &Path) -> Result<EntryState, SandboxError> {
    let unreadable = |error: io::Error| {
        unguardable(
            path,
            &format!("cannot be read ({error})"),
            "make it readable",
        )
    };
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(EntryState::Absent),
        Err(error) => Err(unreadable(error)),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(path).map_err(unreadable)?;
            path.canonicalize().map_err(|error| {
                let problem = if error.kind() == io::ErrorKind::NotFound {
                    String::from("does not exist")
                } else {
                    format!("cannot be resolved ({error})")
                };
                unguardable(
                    path,
                    &format!("is a symlink to '{}', which {problem}", target.display()),
                    "create the target or remove the link",
                )
            })?;
            Ok(EntryState::Symlink(target))
        }
        Ok(metadata) => Ok(EntryState::Present {
            directory: metadata.is_dir(),
        }),
    }
}

fn unguardable(path: &Path, problem: &str, fix: &str) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "'{}' {problem}. Tell the user so they can {fix}, or rerun this script with `permissions=no-sandbox`",
        path.display()
    ))
}

/// A read-only mount or a Seatbelt rule covers one path, so another hard link to the same
/// file would stay writable.
#[cfg(unix)]
fn reject_hard_links(file: &Path) -> Result<(), SandboxError> {
    use std::os::unix::fs::MetadataExt;

    if let Ok(metadata) = std::fs::metadata(file)
        && metadata.is_file()
        && metadata.nlink() > 1
    {
        return Err(unguardable(
            file,
            &format!("has {} hard links", metadata.nlink()),
            "replace the extra links with copies or symlinks",
        ));
    }
    Ok(())
}

fn restore_state(path: &Path, state: &EntryState, link_to_directory: bool) -> io::Result<()> {
    match state {
        EntryState::Absent => {
            if std::fs::symlink_metadata(path).is_ok() {
                remove_entry(path)?;
            }
        }
        EntryState::Symlink(target) => {
            if std::fs::read_link(path).ok().as_ref() != Some(target) {
                if std::fs::symlink_metadata(path).is_ok() {
                    remove_entry(path)?;
                }
                symlink(target, path, link_to_directory)?;
            }
        }
        EntryState::Present { directory } => {
            // A link or junction in place of the entry would redirect the restore.
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && (metadata.file_type().is_symlink() || metadata.is_dir() != *directory)
            {
                remove_entry(path)?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, path: &Path, _directory: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(windows)]
fn symlink(target: &Path, path: &Path, directory: bool) -> io::Result<()> {
    if directory {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    }
}

/// Resolves `path`, following a link whose target is missing to where that target belongs.
#[cfg(windows)]
fn location(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Ok(target) = std::fs::read_link(path) {
                return location(&path.parent().unwrap_or(path).join(target));
            }
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(error);
            };
            Ok(parent.canonicalize()?.join(name))
        }
        result => result,
    }
}

#[cfg(windows)]
fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
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
