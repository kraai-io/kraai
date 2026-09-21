use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;

pub(crate) fn test_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "agent-persistence-{name}-{nanos}-{}",
        Ulid::generate()
    ))
}
