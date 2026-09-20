use bytes::Bytes;
use color_eyre::eyre::{Result, bail};
use reqwest::Method;
use reqwest::header::{HeaderName, HeaderValue};
use tokio::io::{AsyncRead, AsyncReadExt};

pub(super) const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct ParsedRequest {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) path: String,
    pub(super) headers: Vec<(String, HeaderValue)>,
    pub(super) body: Bytes,
}

pub(super) async fn read_request(stream: &mut (impl AsyncRead + Unpin)) -> Result<ParsedRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            bail!("proxy request headers exceed limit");
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("proxy client disconnected before request headers completed");
        }
        let search_start = bytes.len().saturating_sub(3);
        bytes.extend_from_slice(chunk.get(..read).unwrap_or_default());
        if let Some(position) = find_header_end(bytes.get(search_start..).unwrap_or_default())
            .map(|position| search_start.saturating_add(position))
        {
            if position >= MAX_HEADER_BYTES {
                bail!("proxy request headers exceed limit");
            }
            break position;
        }
    };
    let mut lines = bytes
        .get(..header_end)
        .unwrap_or_default()
        .split_inclusive(|byte| *byte == b'\n')
        .map(|line| {
            if line.ends_with(b"\n") {
                line.strip_suffix(b"\r\n")
                    .ok_or_else(|| color_eyre::eyre::eyre!("invalid proxy request line ending"))
            } else {
                Ok(line)
            }
        });
    let request_line = lines
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing request line"))??;
    if !request_line.is_ascii() {
        bail!("invalid proxy request line");
    }
    let request_line = std::str::from_utf8(request_line)?;
    let mut request_parts = request_line.split(' ');
    let method = request_parts.next().unwrap_or_default().to_owned();
    let target = request_parts.next().unwrap_or_default().to_owned();
    let version = request_parts.next().unwrap_or_default();
    if Method::from_bytes(method.as_bytes()).is_err()
        || !target.starts_with('/')
        || target.bytes().any(|byte| byte <= b' ' || byte == 0x7f)
        || version != "HTTP/1.1"
        || request_parts.next().is_some()
    {
        bail!("invalid proxy request line");
    }
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    for line in lines {
        let line = line?;
        let separator = line
            .iter()
            .position(|byte| *byte == b':')
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid proxy request header"))?;
        let (name, value) = line.split_at(separator);
        let name = HeaderName::from_bytes(name)?.as_str().to_owned();
        let value = HeaderValue::from_bytes(trim_header_whitespace(
            value.strip_prefix(b":").unwrap_or_default(),
        ))?;
        if name == "transfer-encoding" {
            bail!("chunked proxy requests are not supported");
        }
        if name == "content-length" {
            content_length = value.to_str()?.parse()?;
            if content_length > MAX_REQUEST_BODY_BYTES {
                bail!("proxy request body exceeds limit");
            }
        }
        headers.push((name, value));
    }
    let body_start = header_end.saturating_add(4);
    let mut body = bytes.get(body_start..).unwrap_or_default().to_vec();
    if body.len() > content_length {
        body.truncate(content_length);
    }
    if body.len() < content_length {
        let missing = content_length - body.len();
        let start = body.len();
        body.resize(content_length, 0);
        stream
            .read_exact(body.get_mut(start..).unwrap_or_default())
            .await?;
        debug_assert_eq!(missing, content_length - start);
    }
    Ok(ParsedRequest {
        method,
        target,
        path,
        headers,
        body: body.into(),
    })
}

fn trim_header_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = bytes {
        bytes = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = bytes {
        bytes = rest;
    }
    bytes
}

pub(super) fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}
