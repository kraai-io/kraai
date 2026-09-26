use std::ops::Range;

use kraai_types::{ContentPart, ImageAttachment, MessageContent};

#[derive(Clone, Debug, Default)]
pub(super) struct DraftImages {
    chips: Vec<ImageChip>,
}

#[derive(Clone, Debug)]
struct ImageChip {
    range: Range<usize>,
    number: usize,
    request_id: u64,
    image: Option<ImageAttachment>,
}

impl DraftImages {
    pub(super) fn len(&self) -> usize {
        self.chips.len()
    }

    pub(super) fn pending(&self) -> bool {
        self.chips.iter().any(|chip| chip.image.is_none())
    }

    pub(super) fn snap(&self, cursor: usize, forward: bool) -> usize {
        self.chips
            .iter()
            .find(|chip| chip.range.start < cursor && cursor < chip.range.end)
            .map_or(cursor, |chip| {
                if forward {
                    chip.range.end
                } else {
                    chip.range.start
                }
            })
    }

    pub(super) fn replace(&mut self, input: &mut String, range: Range<usize>, text: &str) -> usize {
        let range = if range.is_empty() {
            let cursor = self.snap(range.start, true);
            cursor..cursor
        } else {
            self.snap(range.start, false)..self.snap(range.end, true)
        };
        self.chips.retain_mut(|chip| {
            if chip.range.start < range.end && range.start < chip.range.end {
                return false;
            }
            if chip.range.start >= range.end {
                chip.range.start = chip.range.start - range.len() + text.len();
                chip.range.end = chip.range.end - range.len() + text.len();
            }
            true
        });
        input.replace_range(range.clone(), text);
        range.start + text.len()
    }

    pub(super) fn insert(
        &mut self,
        input: &mut String,
        cursor: usize,
        request_id: u64,
        image: Option<ImageAttachment>,
    ) -> usize {
        let number = self.chips.len() + 1;
        let label = format!("[Image #{number}]");
        let end = self.replace(input, cursor..cursor, &label);
        self.chips.push(ImageChip {
            range: end - label.len()..end,
            number,
            request_id,
            image,
        });
        self.chips.sort_by_key(|chip| chip.range.start);
        self.renumber(input, end)
    }

    pub(super) fn renumber(&mut self, input: &mut String, mut cursor: usize) -> usize {
        let mut removed = 0;
        let mut added = 0;
        for (index, chip) in self.chips.iter_mut().enumerate() {
            let start = chip.range.start - removed + added;
            let end = chip.range.end - removed + added;
            chip.number = index + 1;
            let label = format!("[Image #{}]", chip.number);
            input.replace_range(start..end, &label);
            if cursor >= end {
                cursor = cursor - (end - start) + label.len();
            } else if cursor > start {
                cursor = start + label.len();
            }
            removed += end - start;
            added += label.len();
            chip.range = start..start + label.len();
        }
        cursor
    }

    pub(super) fn finish(&mut self, request_id: u64, image: ImageAttachment) -> bool {
        let Some(chip) = self
            .chips
            .iter_mut()
            .find(|chip| chip.request_id == request_id && chip.image.is_none())
        else {
            return false;
        };
        chip.image = Some(image);
        true
    }

    pub(super) fn pending_range(&self, request_id: u64) -> Option<Range<usize>> {
        self.chips
            .iter()
            .find(|chip| chip.request_id == request_id && chip.image.is_none())
            .map(|chip| chip.range.clone())
    }

    pub(super) fn content(&self, input: &str) -> MessageContent {
        let mut parts = Vec::new();
        let mut offset = 0;
        for chip in &self.chips {
            if let Some(text) = input
                .get(offset..chip.range.start)
                .filter(|text| !text.is_empty())
            {
                parts.push(ContentPart::Text { text: text.into() });
            }
            if let Some(image) = &chip.image {
                parts.push(ContentPart::Image {
                    image: image.clone(),
                });
            }
            offset = chip.range.end;
        }
        if let Some(text) = input.get(offset..).filter(|text| !text.is_empty()) {
            parts.push(ContentPart::Text { text: text.into() });
        }
        MessageContent(parts)
    }

    pub(super) fn from_content(content: MessageContent) -> (String, Self) {
        let mut input = String::new();
        let mut images = Self::default();
        for part in content.0 {
            match part {
                ContentPart::Text { text } => input.push_str(&text),
                ContentPart::Image { image } => {
                    let cursor = input.len();
                    images.insert(&mut input, cursor, 0, Some(image));
                }
            }
        }
        (input, images)
    }

    pub(super) fn append_draft(&mut self, input: &mut String, text: &str, images: &Self) {
        let mut offset = 0;
        for chip in &images.chips {
            input.push_str(text.get(offset..chip.range.start).unwrap_or_default());
            let cursor = input.len();
            self.insert(input, cursor, chip.request_id, chip.image.clone());
            offset = chip.range.end;
        }
        input.push_str(text.get(offset..).unwrap_or_default());
    }

    pub(super) fn edited(&self, input: &str) -> Result<Self, &'static str> {
        let mut images = self.clone();
        for chip in &mut images.chips {
            let label = format!("[Image #{}]", chip.number);
            let mut matches = input.match_indices(&label);
            chip.range = matches
                .next()
                .map_or(0..0, |(start, _)| start..start + label.len());
            if matches.next().is_some() {
                return Err("Each image chip can appear only once in the edited prompt");
            }
        }
        images.chips.retain(|chip| !chip.range.is_empty());
        images.chips.sort_by_key(|chip| chip.range.start);
        Ok(images)
    }
}
