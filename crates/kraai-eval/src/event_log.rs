use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::eyre::Result;

pub(crate) struct EventLog {
    file: File,
}

impl EventLog {
    pub(crate) fn new(path: PathBuf) -> Result<Self> {
        Ok(Self {
            file: File::create(path)?,
        })
    }

    pub(crate) fn append(path: PathBuf) -> Result<Self> {
        Ok(Self {
            file: fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        })
    }

    pub(crate) fn write(&mut self, event: &str, data: serde_json::Value) -> Result<()> {
        let timestamp_ms = unix_timestamp_ms()?;
        serde_json::to_writer(
            &mut self.file,
            &serde_json::json!({"timestamp_ms": timestamp_ms, "event": event, "data": data}),
        )?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

pub(crate) fn unix_timestamp_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}
