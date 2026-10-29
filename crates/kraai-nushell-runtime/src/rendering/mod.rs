mod diagnostic;
mod marker;
mod splice;
mod toon;
mod value;

use serde::{Deserialize, Serialize};

pub(crate) use diagnostic::{Diagnostic, Origin};
pub(crate) use marker::Marker;
pub(crate) use splice::render;
pub(crate) use value::OutputValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum OutputFrame {
    Value { id: u64, value: OutputValue },
    Items { list: u64, values: Vec<OutputValue> },
    ListEnd { id: u64, list: u64 },
    Diagnostic { id: u64, diagnostic: Diagnostic },
}
