use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_POINTER_BYTES: u64 = 8192;

pub(crate) fn linked_worktree_common_dir(workspace: &Path) -> Option<PathBuf> {
    let workspace = workspace.canonicalize().ok()?;
    let dot_git = workspace.join(".git");
    if !std::fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let git_dir = resolve_pointer(&workspace, &dot_git, "gitdir: ")?;
    let common_dir = resolve_pointer(&git_dir, &git_dir.join("commondir"), "")?;
    let worktree = resolve_pointer(&git_dir, &git_dir.join("gitdir"), "")?;
    let worktrees = git_dir.parent()?;
    (worktree == dot_git
        && worktrees.file_name()? == "worktrees"
        && worktrees.parent()? == common_dir
        && !common_dir.starts_with(&workspace))
    .then_some(common_dir)
}

fn resolve_pointer(base: &Path, file: &Path, prefix: &str) -> Option<PathBuf> {
    let mut contents = String::new();
    std::fs::File::open(file)
        .ok()?
        .take(MAX_POINTER_BYTES)
        .read_to_string(&mut contents)
        .ok()?;
    let target = contents.strip_prefix(prefix)?.trim_end();
    if target.is_empty() {
        return None;
    }
    base.join(target).canonicalize().ok()
}
