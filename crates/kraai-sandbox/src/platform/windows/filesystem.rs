use std::path::{Path, PathBuf};

use crate::SandboxError;

const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;
const FILE_SUPPORTS_OPEN_BY_FILE_ID: u32 = 0x0100_0000;

pub(super) struct Volume {
    pub(super) filesystem: String,
    pub(super) flags: u32,
}

pub(super) fn check_with(
    path: &Path,
    query: impl FnOnce(&Path) -> Result<Volume, SandboxError>,
) -> Result<(), SandboxError> {
    check_volume(
        path,
        query(path)?,
        FILE_PERSISTENT_ACLS | FILE_SUPPORTS_OPEN_BY_FILE_ID,
    )
}

fn check_volume(path: &Path, volume: Volume, required: u32) -> Result<(), SandboxError> {
    let missing = [
        (
            FILE_PERSISTENT_ACLS,
            "persistent ACLs (FILE_PERSISTENT_ACLS)",
        ),
        (
            FILE_SUPPORTS_OPEN_BY_FILE_ID,
            "file-ID reopening (FILE_SUPPORTS_OPEN_BY_FILE_ID)",
        ),
    ]
    .into_iter()
    .filter_map(|(flag, name)| (required & flag != 0 && volume.flags & flag == 0).then_some(name))
    .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(SandboxError::SandboxUnavailable(format!(
        "cannot secure '{}': filesystem '{}' lacks {}. Move or reconfigure the workspace, runtime root, or temporary directory on a filesystem supporting these Windows sandbox security features, such as NTFS; sandboxing remains required",
        path.display(),
        if volume.filesystem.is_empty() {
            "unknown"
        } else {
            &volume.filesystem
        },
        missing.join(" and "),
    )))
}

pub(super) fn check_roots_with(
    workspace: &Path,
    runtime_roots: &[PathBuf],
    temp: &Path,
    mut query: impl FnMut(&Path) -> Result<Volume, SandboxError>,
) -> Result<(), SandboxError> {
    for root in std::iter::once(workspace)
        .chain(runtime_roots.iter().map(PathBuf::as_path))
        .chain(std::iter::once(temp))
    {
        check_with(root, &mut query)?;
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn check_roots(
    workspace: &Path,
    runtime_roots: &[PathBuf],
    temp: &Path,
) -> Result<(), SandboxError> {
    check_roots_with(workspace, runtime_roots, temp, query)
}

#[cfg(all(windows, test))]
pub(super) fn check(path: &Path) -> Result<(), SandboxError> {
    check_with(path, query)
}

#[cfg(windows)]
pub(super) fn check_private_temp(path: &Path) -> Result<(), SandboxError> {
    check_volume(path, query(path)?, FILE_PERSISTENT_ACLS)
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "query the containing volume through the Windows filesystem APIs"
)]
fn query(path: &Path) -> Result<Volume, SandboxError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};

    if !path.is_absolute() {
        return Err(SandboxError::SandboxUnavailable(format!(
            "filesystem capability query requires an absolute path: '{}'",
            path.display()
        )));
    }
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    if path_wide
        .iter()
        .take(path_wide.len() - 1)
        .any(|unit| *unit == 0)
    {
        return Err(SandboxError::SandboxUnavailable(format!(
            "filesystem capability query path contains NUL: '{}'",
            path.display()
        )));
    }
    let mut root = vec![0_u16; 32768];
    if unsafe { GetVolumePathNameW(path_wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0
    {
        return Err(query_error(
            path,
            "GetVolumePathNameW",
            std::io::Error::last_os_error(),
        ));
    }
    let mut name = [0_u16; 261];
    let mut flags = 0;
    if unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } == 0
    {
        return Err(query_error(
            path,
            "GetVolumeInformationW",
            std::io::Error::last_os_error(),
        ));
    }
    let name = name
        .into_iter()
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    Ok(Volume {
        filesystem: String::from_utf16_lossy(&name),
        flags,
    })
}

fn query_error(path: &Path, api: &str, error: std::io::Error) -> SandboxError {
    SandboxError::SandboxUnavailable(format!(
        "unable to query filesystem capabilities for '{}' using {api}: {error}",
        path.display()
    ))
}

#[cfg(test)]
#[path = "filesystem/tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "filesystem/integration_tests.rs"]
mod integration_tests;
