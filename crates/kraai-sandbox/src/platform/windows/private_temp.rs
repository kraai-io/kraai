use std::path::Path;

pub(crate) fn create(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("temporary directory has no parent"))?;
    super::filesystem::check_private_temp(parent).map_err(std::io::Error::other)?;
    kraai_io::fs::create_private_dir(path)
}
