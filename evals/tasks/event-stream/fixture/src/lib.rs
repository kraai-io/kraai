#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event: String,
    pub id: Option<String>,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    InvalidUtf8,
    Finished,
}

#[derive(Default)]
pub struct Decoder {
    closed: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, _bytes: &[u8]) -> Result<Vec<Event>, DecodeError> {
        if self.closed {
            return Err(DecodeError::Finished);
        }
        Ok(Vec::new())
    }

    pub fn finish(&mut self) -> Result<Vec<Event>, DecodeError> {
        if self.closed {
            return Err(DecodeError::Finished);
        }
        self.closed = true;
        Ok(Vec::new())
    }

    pub fn last_event_id(&self) -> Option<&str> {
        None
    }
}
