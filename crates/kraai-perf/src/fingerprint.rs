use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use color_eyre::eyre::Result;
use sha2::{Digest, Sha256};

pub fn sources(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    encode(hash)
}

pub fn file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(encode(hash));
        }
        hash.update(buffer.get(..read).unwrap_or_default());
    }
}

fn encode(hash: Sha256) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in hash.finalize() {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}
