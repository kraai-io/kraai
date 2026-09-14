mod codec;
mod journal;
mod replay;
mod store;

pub use codec::encode_record;
pub use journal::Journal;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Put { key: String, value: String },
    Delete { key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub id: u64,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalError {
    InvalidRecord { offset: usize },
    Conflict(u64),
}
