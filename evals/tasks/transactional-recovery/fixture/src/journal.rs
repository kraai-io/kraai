use crate::state::State;
use crate::{Checkpoint, Commit, Error, Transaction, encode_transaction, replay};
use std::collections::BTreeMap;

pub struct Journal {
    bytes: Vec<u8>,
    state: State,
}

impl Journal {
    pub fn open(checkpoint: Option<&Checkpoint>, bytes: &[u8]) -> Result<Self, Error> {
        let state = checkpoint.map(|c| c.state.clone()).unwrap_or_default();
        let (state, _committed) = replay::recover(state, bytes)?;
        Ok(Self {
            bytes: bytes.to_vec(),
            state,
        })
    }

    pub fn commit(&mut self, tx: Transaction) -> Result<Commit, Error> {
        let result = self.state.apply(&tx)?;
        if result.applied {
            self.bytes.extend(encode_transaction(&tx));
        }
        Ok(result)
    }

    pub fn checkpoint(&self) -> Checkpoint {
        let mut state = self.state.clone();
        state.receipts.clear();
        Checkpoint { state }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.state.values.get(key).map(String::as_str)
    }

    pub fn version(&self, key: &str) -> Option<u64> {
        self.state.versions.get(key).copied()
    }

    pub fn revision(&self) -> u64 {
        self.state.revision
    }

    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.state.values.clone()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
