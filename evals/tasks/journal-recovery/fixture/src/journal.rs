use std::collections::BTreeMap;

use crate::{Delivery, JournalError, encode_record, replay, store::Store};

pub struct Journal {
    bytes: Vec<u8>,
    store: Store,
}

impl Journal {
    pub fn open(bytes: &[u8]) -> Result<Self, JournalError> {
        let (store, _committed) = replay::recover(bytes)?;
        Ok(Self {
            bytes: bytes.to_vec(),
            store,
        })
    }

    pub fn append(&mut self, delivery: Delivery) -> Result<bool, JournalError> {
        let applied = self.store.apply(&delivery)?;
        if applied {
            self.bytes.extend(encode_record(&delivery));
        }
        Ok(applied)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.store.values.get(key).map(String::as_str)
    }

    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.store.values.clone()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
