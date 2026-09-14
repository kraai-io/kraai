use crate::{JournalError, codec, store::Store};

pub(crate) fn recover(bytes: &[u8]) -> Result<(Store, usize), JournalError> {
    let mut store = Store::default();
    let mut offset = 0;
    while let Some(header) = bytes.get(offset..offset + 4) {
        let length = u32::from_le_bytes(header.try_into().unwrap()) as usize;
        let Some(end) = (offset + 4).checked_add(length) else {
            break;
        };
        let Some(payload) = bytes.get(offset + 4..end) else {
            break;
        };
        let delivery =
            codec::decode_payload(payload).ok_or(JournalError::InvalidRecord { offset })?;
        store.restore(&delivery);
        offset = end;
    }
    Ok((store, offset))
}
