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
