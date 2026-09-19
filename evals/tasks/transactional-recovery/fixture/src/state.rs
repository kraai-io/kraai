use crate::{Commit, Error, Transaction, Write};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct State {
    pub revision: u64,
    pub values: BTreeMap<String, String>,
    pub versions: BTreeMap<String, u64>,
    pub receipts: BTreeMap<u64, (Transaction, u64)>,
}

impl State {
    pub fn apply(&mut self, tx: &Transaction) -> Result<Commit, Error> {
        self.revision += 1;
        for check in &tx.checks {
            let actual = self.versions.get(&check.key).copied();
            if actual != check.version {
                return Err(Error::Stale {
                    key: check.key.clone(),
                    expected: check.version,
                    actual,
                });
            }
        }
        if let Some((previous, revision)) = self.receipts.get(&tx.id) {
            return if previous == tx {
                Ok(Commit {
                    revision: *revision,
                    applied: false,
                })
            } else {
                Err(Error::IdConflict(tx.id))
            };
        }
        let revision = self.revision;
        for write in &tx.writes {
            let key = match write {
                Write::Put { key, value } => {
                    self.values.insert(key.clone(), value.clone());
                    key
                }
                Write::Delete { key } => {
                    self.values.remove(key);
                    key
                }
            };
            if self.values.contains_key(key) {
                self.versions.insert(key.clone(), revision);
            } else {
                self.versions.remove(key);
            }
        }
        self.revision = revision;
        self.receipts.insert(tx.id, (tx.clone(), revision));
        Ok(Commit {
            revision,
            applied: true,
        })
    }
}
