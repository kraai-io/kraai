use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    ExtendedFileIdType, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileIdInfo, FileStandardInfo,
    GetFileInformationByHandleEx, OpenFileById, READ_CONTROL, WRITE_DAC,
};

pub(super) fn ensure_single_link(file: &File) -> io::Result<()> {
    let mut info = FILE_STANDARD_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut info).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if !info.Directory && info.NumberOfLinks != 1 {
        return Err(io::Error::other(
            "cannot grant sandbox access to a file with multiple hard links",
        ));
    }
    Ok(())
}

pub(super) fn open_by_id(pin: &File) -> io::Result<File> {
    let mut info = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            pin.as_raw_handle(),
            FileIdInfo,
            (&raw mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let id = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: ExtendedFileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 {
            ExtendedFileId: info.FileId,
        },
    };
    // ID opens retain the object without pinning its directory entry against ancestor renames.
    // The original pin stays open until this handle owns the same object.
    let handle = unsafe {
        OpenFileById(
            pin.as_raw_handle(),
            &id,
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}
