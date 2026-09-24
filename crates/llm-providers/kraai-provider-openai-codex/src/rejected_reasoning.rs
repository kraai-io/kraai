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

    pub(crate) fn reject(&mut self, items: &[ResponsesRequestItem]) {
        for hash in items.iter().filter_map(fingerprint) {
            if self.fingerprints.insert(hash) {
                self.order.push_back(hash);
                if self.order.len() > CAPACITY
                    && let Some(old) = self.order.pop_front()
                {
                    self.fingerprints.remove(&old);
                }
            }
        }
    }
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
            rejected.reject(&[item(id)]);
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
