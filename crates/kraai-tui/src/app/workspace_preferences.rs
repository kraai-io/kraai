use std::path::PathBuf;

use color_eyre::eyre::Result;
pub(super) use kraai_persistence::WorkspacePreferences;
use kraai_persistence::WorkspacePreferencesStore;

pub(super) fn load_for_current_workspace() -> Result<WorkspacePreferences> {
    let (store, workspace) = store_for_current_workspace()?;
    store.load(&workspace)
}

pub(super) fn save_for_current_workspace(preferences: &WorkspacePreferences) -> Result<()> {
    let (store, workspace) = store_for_current_workspace()?;
    store.save(&workspace, preferences)
}

pub(super) fn current_workspace() -> std::io::Result<PathBuf> {
    std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .or_else(|_| std::env::current_dir())
}

fn store_for_current_workspace() -> Result<(WorkspacePreferencesStore, PathBuf)> {
    let workspace = current_workspace()?;
    let store = WorkspacePreferencesStore::new(&kraai_persistence::agent_state_root()?);
    Ok((store, workspace))
}
