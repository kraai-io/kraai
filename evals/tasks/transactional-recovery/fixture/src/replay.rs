use crate::{Error, codec, state::State};

pub(crate) fn recover(mut state: State, bytes: &[u8]) -> Result<(State, usize), Error> {
    let mut offset = 0;
    while bytes.len() - offset >= 4 {
        let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let start = offset + 4;
        if len > bytes.len() - start {
            break;
        }
        let end = start + len;
        let tx =
            codec::decode_payload(&bytes[start..end]).ok_or(Error::InvalidRecord { offset })?;
        let _ = state.apply(&tx);
        offset = end;
    }
    Ok((state, offset))
}
