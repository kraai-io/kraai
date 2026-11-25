use std::cell::Cell;

use crate::app::line_edit::LineEdit;
use crate::app::model_options::ModelSettingsTarget;

#[derive(Default)]
pub(in crate::app) struct Sheets {
    pub(in crate::app) query: LineEdit,
    pub(in crate::app) index: usize,
    pub(in crate::app) scroll: Cell<usize>,
    pub(in crate::app) notice: Option<String>,
    pub(in crate::app) armed: bool,
    pub(in crate::app) feedback: Option<String>,
    pub(in crate::app) model_settings: Option<ModelSettingsTarget>,
}

impl Sheets {
    pub(in crate::app) fn reset_selection(&mut self) {
        self.index = 0;
        self.scroll.set(0);
    }
}
