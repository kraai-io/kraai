use std::collections::{HashSet, VecDeque};

use sha2::{Digest, Sha256};

use crate::messages::ResponsesRequestItem;

const CAPACITY: usize = 4096;

#[derive(Default)]
pub(crate) struct RejectedReasoning {
    fingerprints: HashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
}

fn fingerprint(item: &ResponsesRequestItem) -> Option<[u8; 32]> {
    let ResponsesRequestItem::Reasoning(payload) = item else {
        return None;
    };
    Some(Sha256::digest(payload.to_string().as_bytes()).into())
}

impl RejectedReasoning {
    pub(crate) fn filter(&self, items: &mut Vec<ResponsesRequestItem>) {
        items
            .retain(|item| fingerprint(item).is_none_or(|hash| !self.fingerprints.contains(&hash)));
    }

    pub(crate) fn reject(&mut self, items: &[ResponsesRequestItem], response: &str) {
        let Some(item) = identified_item(items, response) else {
            return;
        };
        if let Some(hash) = fingerprint(item)
            && self.fingerprints.insert(hash)
        {
            self.order.push_back(hash);
            if self.order.len() > CAPACITY
                && let Some(old) = self.order.pop_front()
            {
                self.fingerprints.remove(&old);
            }
        }
    }
}

fn identified_item<'a>(
    items: &'a [ResponsesRequestItem],
    response: &str,
) -> Option<&'a ResponsesRequestItem> {
    let error: serde_json::Value = serde_json::from_str(response).ok()?;
    if let Some(param) = error
        .pointer("/error/param")
        .and_then(serde_json::Value::as_str)
        && let Some(index) = param
            .strip_prefix("input[")
            .and_then(|value| value.strip_suffix("].encrypted_content"))
            .and_then(|value| value.parse::<usize>().ok())
    {
        return items
            .get(index)
            .filter(|item| matches!(item, ResponsesRequestItem::Reasoning(_)));
    }
    let message = error.pointer("/error/message")?.as_str()?;
    let mut matches = items.iter().filter(|item| {
        let ResponsesRequestItem::Reasoning(payload) = item else {
            return false;
        };
        ["id", "encrypted_content"].iter().any(|field| {
            payload
                .get(field)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| {
                    !value.is_empty()
                        && message
                            .split(|c: char| {
                                !c.is_ascii_alphanumeric()
                                    && !matches!(c, '_' | '-' | '=' | '/' | '+')
                            })
                            .any(|token| token == value)
                })
        })
    });
    let item = matches.next()?;
    matches.next().is_none().then_some(item)
}

#[cfg(test)]
mod tests;
