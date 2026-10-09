//! Fixed-capacity sample ring for UI graphs (spec `03-resources.md` §2:
//! graphs keep 300 samples in a ring buffer).

use std::collections::VecDeque;

/// Bounded FIFO of samples: `push` evicts the oldest entry once full, `iter`
/// yields oldest → newest. Capacity is clamped to at least 1.
#[derive(Debug, Clone)]
pub struct SampleRing<T> {
    slots: VecDeque<T>,
    capacity: usize,
}

impl<T> SampleRing<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Appends a sample, evicting the oldest when at capacity.
    pub fn push(&mut self, sample: T) {
        if self.slots.len() == self.capacity {
            self.slots.pop_front();
        }
        self.slots.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Oldest → newest.
    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, T> {
        self.slots.iter()
    }

    /// Newest sample, if any.
    pub fn newest(&self) -> Option<&T> {
        self.slots.back()
    }
}

impl<T> Default for SampleRing<T> {
    /// Default capacity is [`super::GRAPH_SAMPLE_CAPACITY`] (300).
    fn default() -> Self {
        Self::new(super::GRAPH_SAMPLE_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_enforced_by_evicting_oldest() {
        let mut ring = SampleRing::new(3);
        for i in 0..5 {
            ring.push(i);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.capacity(), 3);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(ring.newest(), Some(&4));
    }

    #[test]
    fn iteration_is_oldest_to_newest() {
        let mut ring = SampleRing::<u32>::default();
        assert!(ring.is_empty());
        for i in 0..400 {
            ring.push(i);
        }
        let seen: Vec<u32> = ring.iter().copied().collect();
        assert_eq!(seen.len(), 300);
        // Oldest surviving sample is the 100th push (0-based).
        assert_eq!(seen.first(), Some(&100));
        assert_eq!(seen.last(), Some(&399));
        // Strictly increasing order.
        assert!(seen.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn default_capacity_matches_graph_samples_limit() {
        assert_eq!(SampleRing::<u8>::default().capacity(), 300);
        let degenerate = SampleRing::<u8>::new(0);
        assert_eq!(degenerate.capacity(), 1);
    }
}
