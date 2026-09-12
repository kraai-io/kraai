use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path};

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_FILE_IS_A_DIRECTORY,
    STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_STOPPED_ON_SYMLINK, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::ScopedReadError;

pub fn open_scoped_file(root: &Path, path: &Path) -> Result<File, ScopedReadError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_error| ScopedReadError::OutsideRoot(path.to_path_buf()))?;
    let relative = relative_name(relative, path)?;
    let root_directory = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(root)
        .map_err(|source| ScopedReadError::OpenRoot {
            path: root.to_path_buf(),
            source,
        })?;
    let metadata = root_directory
        .metadata()
        .map_err(|source| ScopedReadError::OpenRoot {
            path: root.to_path_buf(),
            source,
        })?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ScopedReadError::OutsideRoot(path.to_path_buf()));
    }
    if !metadata.is_dir() {
        return Err(ScopedReadError::OpenRoot {
            path: root.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::NotADirectory,
                "authorized root is not a directory",
            ),
        });
    }
    let file = open_relative(&root_directory, relative, path)?;
    let metadata = file.metadata().map_err(|source| ScopedReadError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ScopedReadError::OutsideRoot(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(ScopedReadError::NotFile(path.to_path_buf()));
    }
    Ok(file)
}

fn relative_name(relative: &Path, requested: &Path) -> Result<Vec<u16>, ScopedReadError> {
    let mut name = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(ScopedReadError::OutsideRoot(requested.to_path_buf()));
        };
        if !name.is_empty() {
            name.push(u16::from(b'\\'));
        }
        for unit in part.encode_wide() {
            if matches!(unit, 0 | 58 | 47 | 92) {
                return Err(ScopedReadError::OutsideRoot(requested.to_path_buf()));
            }
            name.push(unit);
        }
    }
    if name.is_empty() {
        return Err(ScopedReadError::NotFile(requested.to_path_buf()));
    }
    Ok(name)
}

#[expect(
    unsafe_code,
    reason = "NtCreateFile resolves a relative name beneath an owned directory handle without following reparse points"
)]
fn open_relative(root: &File, mut name: Vec<u16>, path: &Path) -> Result<File, ScopedReadError> {
    let length = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| ScopedReadError::Open {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "relative path is too long"),
        })?;
    let name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.as_raw_handle(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: std::ptr::null_mut(),
        SecurityQualityOfService: std::ptr::null_mut(),
    };
    let mut handle = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: All pointers remain valid through this synchronous call. The relative
    // name rejects traversal, and OBJ_DONT_REPARSE stops junctions and symlinks.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_READ,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(match status {
            STATUS_REPARSE_POINT_ENCOUNTERED | STATUS_STOPPED_ON_SYMLINK => {
                ScopedReadError::OutsideRoot(path.to_path_buf())
            }
            STATUS_FILE_IS_A_DIRECTORY => ScopedReadError::NotFile(path.to_path_buf()),
            _ => {
                // SAFETY: RtlNtStatusToDosError maps a scalar status code.
                let source =
                    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32);
                if source.kind() == io::ErrorKind::NotFound {
                    ScopedReadError::NotFound(path.to_path_buf())
                } else {
                    ScopedReadError::Open {
                        path: path.to_path_buf(),
                        source,
                    }
                }
            }
        });
    }
    // SAFETY: Successful synchronous NtCreateFile returns a new owned file handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[expect(
    unsafe_code,
    reason = "MoveFileExW provides atomic non-replacing moves and write-through replacement on Windows"
)]
pub(crate) fn rename(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: Both paths are valid terminated UTF-16 strings for the duration of the call.
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
