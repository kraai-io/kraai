use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf, Prefix};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FileIdInfo,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, VOLUME_NAME_NT,
};

pub(crate) fn canonicalize(cwd: &Path, path: &Path) -> io::Result<PathBuf> {
    let Some(Component::Prefix(prefix)) = cwd.components().next() else {
        return Err(unmapped());
    };
    let (Prefix::Disk(drive) | Prefix::VerbatimDisk(drive)) = prefix.kind() else {
        return Err(unmapped());
    };
    let cwd_file = open(cwd)?;
    let path_file = open(path)?;
    let cwd_name = final_name(&cwd_file)?;
    let path_name = final_name(&path_file)?;
    let (cwd_volume, cwd_tail) = split_volume(&cwd_name)?;
    let (path_volume, path_tail) = split_volume(&path_name)?;
    let dos_tail = cwd.components().skip(2).collect::<PathBuf>();
    if cwd_volume != path_volume
        || !cwd_tail
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&dos_tail.as_os_str().to_string_lossy())
    {
        return Err(unmapped());
    }
    let mapped = PathBuf::from(format!("\\\\?\\{}:\\", char::from(drive))).join(path_tail);
    if file_id(&path_file)? != file_id(&open(&mapped)?)? {
        return Err(unmapped());
    }
    Ok(mapped)
}

fn split_volume(path: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || components.next() != Some(Component::Normal("Device".as_ref()))
    {
        return Err(unmapped());
    }
    let Some(Component::Normal(volume)) = components.next() else {
        return Err(unmapped());
    };
    Ok((PathBuf::from(volume), components.collect()))
}

fn unmapped() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "cannot verify the file's DOS volume mapping",
    )
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[expect(
    unsafe_code,
    reason = "verify that the mapped path names the same opened file"
)]
fn file_id(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((id.VolumeSerialNumber, id.FileId.Identifier))
}

#[expect(
    unsafe_code,
    reason = "query the resolved NT path without accessing the volume manager"
)]
fn final_name(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_NT,
            )
        } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            buffer.truncate(length);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        if length > 32768 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resolved path exceeds the Windows path limit",
            ));
        }
        buffer.resize(length + 1, 0);
    }
}
