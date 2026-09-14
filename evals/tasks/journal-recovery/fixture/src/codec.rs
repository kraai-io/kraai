use crate::{Delivery, Operation};

pub fn encode_record(delivery: &Delivery) -> Vec<u8> {
    let mut payload = delivery.id.to_le_bytes().to_vec();
    let (tag, key, value) = match &delivery.operation {
        Operation::Put { key, value } => (1, key, Some(value)),
        Operation::Delete { key } => (2, key, None),
    };
    payload.push(tag);
    payload.extend_from_slice(&(key.len() as u32).to_le_bytes());
    payload.extend_from_slice(key.as_bytes());
    if let Some(value) = value {
        payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
        payload.extend_from_slice(value.as_bytes());
    }
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend(payload);
    frame
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    let value = input.get(..len)?;
    *input = &input[len..];
    Some(value)
}

fn string(input: &mut &[u8]) -> Option<String> {
    let len = u32::from_le_bytes(take(input, 4)?.try_into().ok()?) as usize;
    std::str::from_utf8(take(input, len)?)
        .ok()
        .map(str::to_owned)
}

pub(crate) fn decode_payload(mut payload: &[u8]) -> Option<Delivery> {
    let id = u64::from_le_bytes(take(&mut payload, 8)?.try_into().ok()?);
    let tag = *take(&mut payload, 1)?.first()?;
    let key = string(&mut payload)?;
    let operation = match tag {
        1 => Operation::Put {
            key,
            value: string(&mut payload)?,
        },
        2 => Operation::Delete { key },
        _ => return None,
    };
    payload.is_empty().then_some(Delivery { id, operation })
}
