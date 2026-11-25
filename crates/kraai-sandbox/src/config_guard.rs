use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
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
    file: Option<&'static str>,
}

type Snapshot = Arc<Vec<GuardedEntry>>;

/// Runs that overlap in one workspace share the snapshot the first of them captured, so a
/// later run never records an earlier run's change as the original.
static ACTIVE: LazyLock<Mutex<HashMap<PathBuf, (usize, Snapshot)>>> = LazyLock::new(Mutex::default);

/// Mounts or Seatbelt rules make the existing entries read-only; [`ConfigGuard::restore`]
/// undoes what they cannot block once the script is gone.
#[derive(Debug)]
pub(crate) struct ConfigGuard {
    workspace: PathBuf,
    entries: Snapshot,
    restored: bool,
}

#[derive(Debug)]
struct GuardedEntry {
    path: PathBuf,
    state: EntryState,
    directory: bool,
    /// Restored on Windows, which has no read-only mounts.
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
        Ok(Self {
            workspace,
            entries,
            restored: false,
        })
    }

    /// Runs after every process of the script has exited. Returns whether a change was undone.
    pub(crate) fn restore(mut self) -> Result<bool, SandboxError> {
        self.restored = true;
        restore_entries(&self.entries)
    }
}

/// Attempts every entry, so one failure cannot leave another entry changed.
fn restore_entries(entries: &[GuardedEntry]) -> Result<bool, SandboxError> {
    // Seatbelt denies every change to the entries, so anything different was changed outside
    // the sandbox.
    if cfg!(target_os = "macos") {
        return Ok(false);
    }
    let mut failure = None;
    let mut undone = false;
    for entry in entries {
        match entry.restore() {
            Ok(changed) => undone |= changed,
            Err(error) => {
                failure.get_or_insert_with(|| {
                    SandboxError::SandboxUnavailable(format!(
                        "unable to undo a sandboxed change to '{}': {error}",
                        entry.path.display()
                    ))
                });
            }
        }
    }
    failure.map_or(Ok(undone), Err)
}

pub fn protected_entry_names() -> impl Iterator<Item = &'static str> {
    PROTECTED.iter().map(|protected| protected.name)
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        if !self.restored {
            let _ = restore_entries(&self.entries);
        }
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((runs, _)) = active.get_mut(&self.workspace) {
            *runs -= 1;
            if *runs == 0 {
                active.remove(&self.workspace);
            }
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn entry_paths(workspace: &Path) -> impl Iterator<Item = PathBuf> {
    PROTECTED
        .iter()
        .map(move |protected| workspace.join(protected.name))
}

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

    fn restore(&self) -> io::Result<bool> {
        let undone = restore_state(&self.path, &self.state, self.directory)?;
        #[cfg(windows)]
        if let Some(file) = &self.file {
            return Ok(file.restore()? || undone);
        }
        Ok(undone)
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

    fn restore(&self) -> io::Result<bool> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let undone = restore_state(&self.path, &self.state, false)?;
        let Some(contents) = &self.contents else {
            return Ok(undone);
        };
        let location = location(&self.path)?;
        if location != self.location {
            return Err(io::Error::other(format!(
                "it now resolves to '{}' instead of '{}'",
                location.display(),
                self.location.display()
            )));
        }
        match read_optional(&self.path)? {
            Some(current) if current == *contents => Ok(undone),
            Some(_) => {
                move_aside(&self.path)?;
                std::fs::write(&self.path, contents)?;
                Ok(true)
            }
            None => {
                std::fs::write(&self.path, contents)?;
                Ok(true)
            }
        }
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
            let resolved = path.canonicalize().map_err(|error| {
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
            if !links_directly(path, &target, &resolved) {
                return Err(unguardable(
                    path,
                    &format!(
                        "is a symlink to '{}', which goes through another symlink",
                        target.display()
                    ),
                    &format!("point the link straight at '{}'", resolved.display()),
                ));
            }
            Ok(EntryState::Symlink(target))
        }
        Ok(metadata) => Ok(EntryState::Present {
            directory: metadata.is_dir(),
        }),
    }
}

/// Protection covers the resolved path, so a script could swap any other link along the way.
fn links_directly(path: &Path, target: &Path, resolved: &Path) -> bool {
    let Some(parent) = path.parent().and_then(|parent| parent.canonicalize().ok()) else {
        return false;
    };
    let mut direct = PathBuf::new();
    for component in parent.join(target).components() {
        match component {
            Component::ParentDir => {
                direct.pop();
            }
            Component::CurDir => {}
            component @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                direct.push(component);
            }
        }
    }
    direct.ancestors().all(|ancestor| {
        std::fs::symlink_metadata(ancestor).is_ok_and(|metadata| !metadata.file_type().is_symlink())
    }) && direct.canonicalize().is_ok_and(|direct| direct == resolved)
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

fn restore_state(path: &Path, state: &EntryState, link_to_directory: bool) -> io::Result<bool> {
    match state {
        EntryState::Absent => {
            if std::fs::symlink_metadata(path).is_ok() {
                move_aside(path)?;
                return Ok(true);
            }
        }
        EntryState::Symlink(target) => {
            if std::fs::read_link(path).ok().as_ref() != Some(target) {
                if std::fs::symlink_metadata(path).is_ok() {
                    move_aside(path)?;
                }
                symlink(target, path, link_to_directory)?;
                return Ok(true);
            }
        }
        EntryState::Present { directory } => {
            // A link or junction in place of the entry would redirect the restore.
            if let Ok(metadata) = std::fs::symlink_metadata(path)
                && (metadata.file_type().is_symlink() || metadata.is_dir() != *directory)
            {
                move_aside(path)?;
                return Ok(true);
            }
        }
    }
    Ok(false)
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

/// Lets the owner open every directory below `path`, which a script may have locked.
#[cfg(unix)]
fn unlock_tree(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    let _ = std::fs::set_permissions(path, permissions);
    for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
        unlock_tree(&entry.path());
    }
}

/// Renames an entry next to itself instead of deleting it, because the restore cannot tell a
/// script's change from one made outside the sandbox during the run.
fn move_aside(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    unlock_tree(path);
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("the entry has no file name"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..100_u32 {
        let mut aside = name.to_os_string();
        aside.push(format!(".sandbox-undone-{stamp}-{attempt}"));
        let aside = path.with_file_name(aside);
        if std::fs::symlink_metadata(&aside).is_err() {
            return std::fs::rename(path, aside);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name to move the entry aside",
    ))
}
