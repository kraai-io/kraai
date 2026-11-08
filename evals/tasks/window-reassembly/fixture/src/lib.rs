use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReassemblyError {
    InvalidWindow(u16),
    OutsideWindow(u16),
}

#[derive(Debug)]
pub struct Reassembler {
    expected: u16,
    window: u16,
    pending: HashMap<u16, Vec<u8>>,
}

impl Reassembler {
    pub fn new(first: u16, window: u16) -> Result<Self, ReassemblyError> {
        if window == 0 {
            return Err(ReassemblyError::InvalidWindow(window));
        }
        Ok(Self {
            expected: first,
            window,
            pending: HashMap::new(),
        })
    }

    pub fn expected(&self) -> u16 {
        self.expected
    }

    pub fn buffered(&self) -> usize {
        self.pending.len()
    }

    pub fn push(&mut self, seq: u16, payload: Vec<u8>) -> Result<Vec<Vec<u8>>, ReassemblyError> {
        if seq < self.expected {
            return Ok(Vec::new());
        }
        if seq >= self.expected.wrapping_add(self.window) {
            return Err(ReassemblyError::OutsideWindow(seq));
        }
        if seq != self.expected {
            self.pending.entry(seq).or_insert(payload);
            return Ok(Vec::new());
        }
        let mut delivered = vec![payload];
        self.expected = self.expected.wrapping_add(1);
        while let Some(next) = self.pending.remove(&self.expected) {
            delivered.push(next);
            self.expected = self.expected.wrapping_add(1);
        }
        Ok(delivered)
    }
}
