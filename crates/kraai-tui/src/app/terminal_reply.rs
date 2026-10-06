use super::{CrosstermEvent, KeyCode, KeyModifiers};

#[derive(Default)]
pub(super) struct TerminalReply {
    pub(super) pending: bool,
    response: String,
    pub(super) held: Vec<CrosstermEvent>,
}

pub(super) enum FilteredEvent {
    Pass(CrosstermEvent),
    Consumed,
    Replay(Vec<CrosstermEvent>),
}

impl TerminalReply {
    #[cfg(test)]
    pub(super) fn pending() -> Self {
        Self {
            pending: true,
            ..Self::default()
        }
    }

    pub(super) fn request(&mut self, query: &[u8], output: &mut impl std::io::Write) {
        self.pending = true;
        let _ = output.write_all(query).and_then(|()| output.flush());
    }

    pub(super) fn filter(
        &mut self,
        event: CrosstermEvent,
        introducer: char,
        valid_prefix: impl Fn(&str) -> bool,
        mut accept: impl FnMut(&str) -> bool,
    ) -> FilteredEvent {
        if !self.pending {
            return FilteredEvent::Pass(event);
        }
        let CrosstermEvent::Key(key) = &event else {
            return FilteredEvent::Pass(event);
        };
        if self.held.is_empty() {
            if key.code == KeyCode::Char(introducer)
                && key.modifiers.difference(KeyModifiers::SHIFT) == KeyModifiers::ALT
            {
                self.held.push(event);
                return FilteredEvent::Consumed;
            }
            return FilteredEvent::Pass(event);
        }
        let terminal = key.code == KeyCode::Char('\\') && key.modifiers == KeyModifiers::ALT
            || introducer == ']'
                && key.code == KeyCode::Char('g')
                && key.modifiers == KeyModifiers::CONTROL;
        if terminal {
            self.held.push(event);
            if accept(&self.response) {
                self.pending = false;
                self.held.clear();
                self.response.clear();
                return FilteredEvent::Consumed;
            }
            return self.replay();
        }
        if let KeyCode::Char(ch) = key.code
            && (key.modifiers.is_empty()
                || key.modifiers == KeyModifiers::SHIFT && ch.is_ascii_uppercase())
        {
            self.response.push(ch);
            self.held.push(event);
            if self.response.len() <= 64 && valid_prefix(&self.response) {
                return FilteredEvent::Consumed;
            }
        } else {
            self.held.push(event);
        }
        self.replay()
    }

    fn replay(&mut self) -> FilteredEvent {
        self.response.clear();
        FilteredEvent::Replay(std::mem::take(&mut self.held))
    }
}
