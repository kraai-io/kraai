#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    InvalidTarget,
    InvalidEscape,
    InvalidUtf8,
    UnsafePath,
}

pub(crate) struct Target {
    pub path: String,
    pub directory: bool,
    pub query: Option<String>,
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn parse(target: &str) -> Result<Target, RouteError> {
    let (raw_path, query) = target
        .split_once('?')
        .map_or((target, None), |(path, query)| {
            (path, Some(query.to_owned()))
        });
    if !raw_path.starts_with('/') || raw_path.contains('#') {
        return Err(RouteError::InvalidTarget);
    }
    if raw_path.split('/').any(|segment| segment == "..") || raw_path.contains(['\0', '\\']) {
        return Err(RouteError::UnsafePath);
    }
    let bytes = raw_path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] == b'%' {
            let high = bytes.get(offset + 1).and_then(|byte| hex(*byte));
            let low = bytes.get(offset + 2).and_then(|byte| hex(*byte));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(RouteError::InvalidEscape);
            };
            decoded.push(high * 16 + low);
            offset += 3;
        } else {
            decoded.push(bytes[offset]);
            offset += 1;
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| RouteError::InvalidUtf8)?;
    let directory = decoded.ends_with('/');
    let segments: Vec<_> = decoded
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    Ok(Target {
        path: format!("/{}", segments.join("/")),
        directory,
        query,
    })
}
