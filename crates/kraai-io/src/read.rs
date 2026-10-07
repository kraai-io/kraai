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
#[expect(
    clippy::unwrap_used,
    reason = "bounded reader tests assert exact read contracts"
)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_exact_limits_and_overflow_without_consuming_the_tail() {
        for (contents, limit, expected, truncated) in [
            (b"".as_slice(), 0, b"".as_slice(), false),
            (b"x".as_slice(), 0, b"".as_slice(), true),
            (b"abc".as_slice(), 3, b"abc".as_slice(), false),
            (b"abcde".as_slice(), 3, b"abc".as_slice(), true),
            (b"abc".as_slice(), u64::MAX, b"abc".as_slice(), false),
        ] {
            let mut reader = contents;
            let actual = read_prefix(&mut reader, limit).unwrap();
            assert_eq!(actual.bytes, expected);
            assert_eq!(actual.truncated, truncated);
            assert_eq!(
                reader.len(),
                contents.len() - expected.len() - usize::from(truncated)
            );
            match read_bounded(contents, limit) {
                Err(ReadLimitError::Exceeded { limit: actual }) => {
                    assert!(truncated);
                    assert_eq!(actual, limit);
                }
                actual => assert_eq!(actual.unwrap(), expected),
            }
        }
    }

    struct InterruptedProbe {
        interrupted: bool,
    }

    impl Read for InterruptedProbe {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            b"x".as_slice().read(buffer)
        }
    }

    #[test]
    fn retries_interrupted_overflow_probe() {
        assert!(
            read_prefix(InterruptedProbe { interrupted: false }, 0)
                .unwrap()
                .truncated
        );
    }

    struct FailedProbe;

    impl Read for FailedProbe {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }

    #[test]
    fn preserves_probe_errors() {
        assert!(matches!(
            read_bounded(FailedProbe, 0),
            Err(ReadLimitError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn async_readers_preserve_bounds() {
        for limit in [0, 1, 3, 4, u64::MAX] {
            let expected = read_prefix(b"abc".as_slice(), limit).unwrap();
            let actual = read_prefix_async(b"abc".as_slice(), limit).await.unwrap();
            assert_eq!(actual, expected);
            assert_eq!(
                read_bounded_async(b"abc".as_slice(), limit).await.is_err(),
                limit < 3
            );
        }
    }
}
