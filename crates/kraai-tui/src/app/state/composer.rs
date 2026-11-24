use crate::app::draft_images::DraftImages;

pub(in crate::app) struct Composer {
    pub(in crate::app) text: String,
    pub(in crate::app) cursor: usize,
    pub(in crate::app) width: u16,
    pub(in crate::app) images: DraftImages,
    pub(in crate::app) next_image_request_id: u64,
    pub(in crate::app) history: Vec<String>,
    pub(in crate::app) history_index: Option<usize>,
    pub(in crate::app) history_draft: Option<(String, DraftImages)>,
    pub(in crate::app) command_filter: Option<String>,
    pub(in crate::app) command_popup_dismissed: bool,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            width: 80,
            images: DraftImages::default(),
            next_image_request_id: 1,
            history: Vec::new(),
            history_index: None,
            history_draft: None,
            command_filter: None,
            command_popup_dismissed: false,
        }
    }
}
