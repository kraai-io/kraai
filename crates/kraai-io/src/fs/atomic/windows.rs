use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

#[expect(
    unsafe_code,
    reason = "MoveFileExW provides atomic non-replacing moves and write-through replacement on Windows"
)]
pub(super) fn rename(
    source: &Path,
    destination: &Path,
    replace: bool,
    durable: bool,
) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    let flags = if durable { MOVEFILE_WRITE_THROUGH } else { 0 }
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: Both paths remain valid terminated UTF-16 strings for the call.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    path.push(0);
    Ok(path)
}
