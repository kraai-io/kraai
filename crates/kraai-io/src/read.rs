use std::io::{self, Read};

#[derive(Debug, PartialEq, Eq)]
pub struct ReadPrefix {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadLimitError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("input exceeds the {limit} byte limit")]
    Exceeded { limit: u64 },
}

pub fn read_prefix(mut reader: impl Read, limit: u64) -> io::Result<ReadPrefix> {
    let mut bytes = Vec::new();
    reader.by_ref().take(limit).read_to_end(&mut bytes)?;
    let truncated = bytes.len() as u64 == limit && read_probe(&mut reader)?;
    Ok(ReadPrefix { bytes, truncated })
}

fn read_probe(reader: &mut impl Read) -> io::Result<bool> {
    loop {
        match reader.read(&mut [0]) {
            Ok(count) => return Ok(count != 0),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

pub fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, ReadLimitError> {
    let prefix = read_prefix(reader, limit)?;
    if prefix.truncated {
        return Err(ReadLimitError::Exceeded { limit });
    }
    Ok(prefix.bytes)
}

#[cfg(feature = "async")]
pub async fn read_prefix_async(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    limit: u64,
) -> io::Result<ReadPrefix> {
    use tokio::io::AsyncReadExt;

    let mut bytes = Vec::new();
    (&mut reader).take(limit).read_to_end(&mut bytes).await?;
    let truncated = if bytes.len() as u64 == limit {
        loop {
            match reader.read(&mut [0]).await {
                Ok(count) => break count != 0,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    } else {
        false
    };
    Ok(ReadPrefix { bytes, truncated })
}

#[cfg(feature = "async")]
pub async fn read_bounded_async(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: u64,
) -> Result<Vec<u8>, ReadLimitError> {
    let prefix = read_prefix_async(reader, limit).await?;
    if prefix.truncated {
        return Err(ReadLimitError::Exceeded { limit });
    }
    Ok(prefix.bytes)
}

#[cfg(test)]
mod tests;
