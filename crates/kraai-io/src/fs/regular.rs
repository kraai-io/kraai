use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalSymlinkPolicy {
    Follow,
    Reject,
}

pub fn open_regular_file(path: &Path, symlinks: FinalSymlinkPolicy) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let mut flags = rustix::fs::OFlags::NONBLOCK;
        if symlinks == FinalSymlinkPolicy::Reject {
            flags |= rustix::fs::OFlags::NOFOLLOW;
        }
        options.custom_flags(flags.bits() as i32);
    }
    #[cfg(windows)]
    if symlinks == FinalSymlinkPolicy::Reject {
        use std::os::windows::fs::OpenOptionsExt;

        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    if symlinks == FinalSymlinkPolicy::Reject {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rejecting final symlinks is unsupported on this platform",
        ));
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    if symlinks == FinalSymlinkPolicy::Reject {
        use std::os::windows::fs::MetadataExt;

        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path is a reparse point",
            ));
        }
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(feature = "async")]
pub async fn open_regular_file_async(
    path: &Path,
    symlinks: FinalSymlinkPolicy,
) -> io::Result<tokio::fs::File> {
    let path = path.to_path_buf();
    let file = tokio::task::spawn_blocking(move || open_regular_file(&path, symlinks))
        .await
        .map_err(io::Error::other)??;
    Ok(tokio::fs::File::from_std(file))
}

pub fn read_regular_text_file(path: &Path) -> io::Result<String> {
    let mut file = open_regular_file(path, FinalSymlinkPolicy::Follow)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    optional(
        open_regular_file(path, FinalSymlinkPolicy::Follow).and_then(|mut file| {
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;
            Ok(contents)
        }),
    )
}

pub fn read_optional_text(path: &Path) -> io::Result<Option<String>> {
    optional(read_regular_text_file(path))
}

#[cfg(feature = "async")]
pub async fn read_optional_async(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_optional(&path))
        .await
        .map_err(io::Error::other)?
}

#[cfg(feature = "async")]
pub async fn read_optional_text_async(path: &Path) -> io::Result<Option<String>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_optional_text(&path))
        .await
        .map_err(io::Error::other)?
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
