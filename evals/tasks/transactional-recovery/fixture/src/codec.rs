use crate::{Check, Transaction, Write};

fn put_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

pub fn encode_transaction(tx: &Transaction) -> Vec<u8> {
    let mut payload = tx.id.to_le_bytes().to_vec();
    payload.extend_from_slice(&(tx.checks.len() as u32).to_le_bytes());
    for check in &tx.checks {
        put_string(&mut payload, &check.key);
        match check.version {
            None => payload.push(0),
            Some(version) => {
                payload.push(1);
                payload.extend_from_slice(&version.to_le_bytes());
            }
        }
    }
    payload.extend_from_slice(&(tx.writes.len() as u32).to_le_bytes());
    for write in &tx.writes {
        match write {
            Write::Put { key, value } => {
                payload.push(1);
                put_string(&mut payload, key);
                put_string(&mut payload, value);
            }
            Write::Delete { key } => {
                payload.push(2);
                put_string(&mut payload, key);
            }
        }
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

fn u32_value(input: &mut &[u8]) -> Option<usize> {
    Some(u32::from_le_bytes(take(input, 4)?.try_into().ok()?) as usize)
}

fn string(input: &mut &[u8]) -> Option<String> {
    let len = u32_value(input)?;
    std::str::from_utf8(take(input, len)?)
        .ok()
        .map(str::to_owned)
}

pub(crate) fn decode_payload(mut input: &[u8]) -> Option<Transaction> {
    let id = u64::from_le_bytes(take(&mut input, 8)?.try_into().ok()?);
    let count = u32_value(&mut input)?;
    let mut checks = Vec::new();
    for _ in 0..count {
        let key = string(&mut input)?;
        let version = match *take(&mut input, 1)?.first()? {
            0 => None,
            1 => Some(u64::from_le_bytes(take(&mut input, 8)?.try_into().ok()?)),
            _ => return None,
        };
        checks.push(Check { key, version });
    }
    let count = u32_value(&mut input)?;
    let mut writes = Vec::new();
    for _ in 0..count {
        let tag = *take(&mut input, 1)?.first()?;
        let key = string(&mut input)?;
        writes.push(match tag {
            1 => Write::Put {
                key,
                value: string(&mut input)?,
            },
            2 => Write::Delete { key },
            _ => return None,
        });
    }
    Some(Transaction { id, checks, writes })
}
