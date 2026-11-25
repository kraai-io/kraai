use std::cell::Cell;

#[derive(Default)]
pub(in crate::app) struct Errors {
    pub(in crate::app) notice: Option<String>,
    pub(in crate::app) last: Option<String>,
    pub(in crate::app) open: bool,
    pub(in crate::app) scroll: Cell<usize>,
}
