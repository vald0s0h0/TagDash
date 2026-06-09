use std::collections::VecDeque;

/// Fixed-capacity circular buffer. Oldest item is evicted when full.
pub struct RingBuffer<T> {
    data: VecDeque<T>,
    capacity: usize,
}

impl<T: Clone> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        // Grow lazily rather than pre-reserving `capacity`: a buffer is created
        // for every (symbol, timeframe) the feed touches — thousands of them — and
        // most hold only a handful of bars, so reserving the full cap up front
        // would waste hundreds of MB. `push` still enforces the cap.
        Self {
            data: VecDeque::new(),
            capacity,
        }
    }

    pub fn push(&mut self, item: T) {
        if self.data.len() >= self.capacity {
            self.data.pop_front();
        }
        self.data.push_back(item);
    }

    pub fn as_vec(&self) -> Vec<T> {
        self.data.iter().cloned().collect()
    }

    /// Replace the entire contents with `items` (oldest → newest), keeping at
    /// most `capacity` of the most recent. Used to splice historical bars in
    /// front of the live ones when backfilling a chart.
    pub fn replace_with(&mut self, items: Vec<T>) {
        let start = items.len().saturating_sub(self.capacity);
        self.data = items.into_iter().skip(start).collect();
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}
