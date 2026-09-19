mod codec;
mod journal;
mod replay;
mod state;

pub use codec::encode_transaction;
pub use journal::Journal;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub key: String,
    pub version: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Put { key: String, value: String },
    Delete { key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub id: u64,
    pub checks: Vec<Check>,
    pub writes: Vec<Write>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub revision: u64,
    pub applied: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    IdConflict(u64),
    Stale {
        key: String,
        expected: Option<u64>,
        actual: Option<u64>,
    },
    InvalidRecord {
        offset: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    state: state::State,
}
