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
mod tests {
    use super::*;
    fn item(id: usize) -> ResponsesRequestItem {
        ResponsesRequestItem::Reasoning(
            serde_json::json!({"id": id, "encrypted_content": format!("opaque-{id}")}),
        )
    }
    #[test]
    fn rejection_memory_is_bounded_and_preserves_new_reasoning() {
        let mut rejected = RejectedReasoning::default();
        for id in 0..=CAPACITY {
            rejected.reject(
                &[item(id)],
                r#"{"error":{"param":"input[0].encrypted_content"}}"#,
            );
        }
        assert_eq!(rejected.fingerprints.len(), CAPACITY);
        assert_eq!(rejected.order.len(), CAPACITY);
        let mut items = vec![item(CAPACITY), item(CAPACITY + 1)];
        rejected.filter(&mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items.first().and_then(fingerprint),
            fingerprint(&item(CAPACITY + 1))
        );
    }
}

#[cfg(test)]
mod identification_tests {
    use super::*;
    #[test]
    fn only_unambiguously_identified_reasoning_is_suppressed() {
        for response in [
            r#"{"error":{"message":"The encrypted content opaque-bad could not be verified."}}"#,
            r#"{"error":{"message":"The encrypted content for item rs-bad could not be verified."}}"#,
            r#"{"error":{"param":"input[1].encrypted_content"}}"#,
        ] {
            let mut items = vec![
                ResponsesRequestItem::Reasoning(
                    serde_json::json!({"id":"rs-good","encrypted_content":"opaque-good"}),
                ),
                ResponsesRequestItem::Reasoning(
                    serde_json::json!({"id":"rs-bad","encrypted_content":"opaque-bad"}),
                ),
            ];
            let mut cache = RejectedReasoning::default();
            cache.reject(&items, response);
            cache.filter(&mut items);
            assert_eq!(items.len(), 1);
            assert!(
                matches!(items.first(), Some(ResponsesRequestItem::Reasoning(payload)) if payload["id"] == "rs-good")
            );
        }
        for message in [
            "could not verify content",
            "opaque-...",
            "rs-good rs-bad",
            "rs-bad-extra",
        ] {
            let mut items = vec![
                ResponsesRequestItem::Reasoning(
                    serde_json::json!({"id":"rs-good","encrypted_content":"opaque-good"}),
                ),
                ResponsesRequestItem::Reasoning(
                    serde_json::json!({"id":"rs-bad","encrypted_content":"opaque-bad"}),
                ),
            ];
            let mut cache = RejectedReasoning::default();
            cache.reject(
                &items,
                &serde_json::json!({"error":{"message":message}}).to_string(),
            );
            cache.filter(&mut items);
            assert_eq!(items.len(), 2);
        }
    }
}
