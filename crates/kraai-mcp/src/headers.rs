use std::collections::BTreeMap;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

pub(crate) fn has_authorization(headers: &BTreeMap<String, String>) -> bool {
    headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("authorization"))
}

pub(crate) fn parse(headers: &BTreeMap<String, String>) -> Result<HeaderMap, String> {
    let mut parsed = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("Invalid HTTP header name: {error}"))?;
        if parsed.contains_key(&name) {
            return Err(format!("Duplicate HTTP header {name}"));
        }
        let mut value = HeaderValue::from_str(value)
            .map_err(|error| format!("Invalid HTTP header value for {name}: {error}"))?;
        value.set_sensitive(true);
        parsed.insert(name, value);
    }
    Ok(parsed)
}
