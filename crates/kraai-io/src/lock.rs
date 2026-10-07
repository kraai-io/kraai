use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
#[cfg(feature = "async")]
use std::time::Duration;

#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

pub fn open_private_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

impl FileLock {
    pub fn try_acquire(file: File) -> Result<Self, std::fs::TryLockError> {
        file.try_lock()?;
        Ok(Self { _file: file })
    }

    pub fn acquire(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self { _file: file })
    }

    #[cfg(feature = "async")]
    pub async fn acquire_until(
        file: File,
        deadline: Option<tokio::time::Instant>,
        poll_interval: Duration,
    ) -> io::Result<Self> {
        if poll_interval.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Lock polling interval must be positive",
            ));
        }
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    let wake = tokio::time::Instant::now() + poll_interval;
                    if let Some(deadline) = deadline {
                        if tokio::time::Instant::now() >= deadline {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "Timed out waiting for file lock",
                            ));
                        }
                        tokio::time::sleep_until(wake.min(deadline)).await;
                    } else {
                        tokio::time::sleep_until(wake).await;
                    }
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests;
