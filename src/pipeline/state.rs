use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct PendingItem {
    pub commit_tick: u32,
    pub revealed_bits: [u8; 512],
    pub committed_digest: [u8; 32],
    pub amount: u64,
}

#[derive(Debug, Default)]
pub struct PendingState {
    items: VecDeque<PendingItem>,
}

impl PendingState {
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
        }
    }

    pub fn push(&mut self, item: PendingItem) {
        self.items.push_back(item);
    }

    pub fn pop_revealable(&mut self, current_tick: u32, delay: u32) -> Option<PendingItem> {
        match self.items.front() {
            Some(item) if current_tick >= item.commit_tick + delay => self.items.pop_front(),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}
