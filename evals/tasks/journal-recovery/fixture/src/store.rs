use std::collections::BTreeMap;

use crate::{Delivery, JournalError, Operation};

#[derive(Default)]
pub(crate) struct Store {
    pub values: BTreeMap<String, String>,
    seen: BTreeMap<u64, Operation>,
}

impl Store {
    fn update(&mut self, operation: &Operation) {
        match operation {
            Operation::Put { key, value } => {
                self.values.insert(key.clone(), value.clone());
            }
            Operation::Delete { key } => {
                self.values.remove(key);
            }
        }
    }

    pub fn apply(&mut self, delivery: &Delivery) -> Result<bool, JournalError> {
        if let Some(previous) = self.seen.get(&delivery.id) {
            return if previous == &delivery.operation {
                Ok(false)
            } else {
                Err(JournalError::Conflict(delivery.id))
            };
        }
        self.update(&delivery.operation);
        self.seen.insert(delivery.id, delivery.operation.clone());
        Ok(true)
    }

    pub fn restore(&mut self, delivery: &Delivery) {
        self.update(&delivery.operation);
        self.seen.insert(delivery.id, delivery.operation.clone());
    }
}
