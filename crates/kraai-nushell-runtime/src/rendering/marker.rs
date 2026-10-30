use serde::{Deserialize, Serialize};

const START: u8 = 0x1e;
const END: u8 = 0x1f;
const NONCE_BYTES: usize = 16;
const MAX_ID_DIGITS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Marker(String);

pub(crate) enum Scan {
    Text(usize),
    Marker { length: usize, id: u64 },
    Incomplete,
}

impl Marker {
    pub(crate) fn random() -> Self {
        Self(
            rand::random::<[u8; NONCE_BYTES]>()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    pub(crate) fn encode(&self, id: u64) -> Vec<u8> {
        let mut bytes = self.prefix();
        bytes.extend(id.to_string().bytes());
        bytes.push(END);
        bytes
    }

    fn prefix(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.0.len() + 3 + MAX_ID_DIGITS);
        bytes.push(START);
        bytes.extend(self.0.bytes());
        bytes.push(b':');
        bytes
    }

    pub(crate) fn scan(&self, bytes: &[u8]) -> Scan {
        let Some(start) = bytes.iter().position(|&byte| byte == START) else {
            return Scan::Text(bytes.len());
        };
        if start != 0 {
            return Scan::Text(start);
        }
        let prefix = self.prefix();
        let compared = bytes.len().min(prefix.len());
        if bytes.get(..compared) != prefix.get(..compared) {
            return Scan::Text(1);
        }
        if bytes.len() < prefix.len() {
            return Scan::Incomplete;
        }
        let digits = bytes.get(prefix.len()..).unwrap_or_default();
        let length = digits
            .iter()
            .take(MAX_ID_DIGITS + 1)
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        match digits.get(length) {
            None if length <= MAX_ID_DIGITS => Scan::Incomplete,
            Some(&END) if length > 0 => {
                std::str::from_utf8(digits.get(..length).unwrap_or_default())
                    .ok()
                    .and_then(|digits| digits.parse().ok())
                    .map_or(Scan::Text(1), |id| Scan::Marker {
                        length: prefix.len() + length + 1,
                        id,
                    })
            }
            _ => Scan::Text(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Marker, Scan};

    #[test]
    fn markers_round_trip_and_partial_markers_wait_for_more_bytes() {
        let marker = Marker::random();
        let encoded = marker.encode(42);
        assert!(matches!(
            marker.scan(&encoded),
            Scan::Marker { length, id: 42 } if length == encoded.len()
        ));
        for split in 1..encoded.len() {
            assert!(matches!(
                marker.scan(encoded.get(..split).unwrap_or_default()),
                Scan::Incomplete
            ));
        }
        assert!(matches!(marker.scan(b"text\x1e"), Scan::Text(4)));
    }

    #[test]
    fn foreign_record_separators_stay_text() {
        let marker = Marker::random();
        let other = Marker::random().encode(1);
        assert!(matches!(marker.scan(&other), Scan::Text(1)));
        let mut broken = marker.encode(1);
        broken.pop();
        broken.push(b'x');
        assert!(matches!(marker.scan(&broken), Scan::Text(1)));
        assert!(matches!(marker.scan(b"\x1e!x"), Scan::Text(1)));
    }
}
