//! A transactional, bounded timeline of original encoded images.
use std::{collections::VecDeque, sync::Arc};
const LIMIT: usize = 100;
const BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub(super) struct Entry {
    pub position: usize,
    pub bytes: Arc<Vec<u8>>,
}
#[derive(Default)]
pub(super) struct Timeline {
    entries: VecDeque<Entry>,
    current: Option<usize>,
    frontier: Option<usize>,
    bytes: usize,
}
impl Timeline {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn position(&self) -> Option<usize> {
        self.current
    }
    pub fn previous(&self) -> Option<usize> {
        let p = self.current?.checked_sub(1)?;
        self.entries.iter().any(|e| e.position == p).then_some(p)
    }
    pub fn at_tip(&self) -> bool {
        self.current == self.frontier
    }
    pub fn get(&self, p: usize) -> Option<Arc<Vec<u8>>> {
        self.entries
            .iter()
            .find(|e| e.position == p)
            .map(|e| e.bytes.clone())
    }
    pub fn commit(&mut self, p: usize, bytes: Arc<Vec<u8>>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.position == p) {
            self.bytes = self.bytes - entry.bytes.len() + bytes.len();
            entry.bytes = bytes;
        } else {
            self.bytes += bytes.len();
            self.entries.push_back(Entry { position: p, bytes });
        }
        self.current = Some(p);
        self.frontier = Some(self.frontier.map_or(p, |old| old.max(p)));
        while self.entries.len() > 1 && (self.entries.len() > LIMIT || self.bytes > BYTES) {
            self.bytes -= self.entries.pop_front().unwrap().bytes.len();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_history_has_a_memory_bound_and_keeps_its_frontier() {
        let mut t = Timeline::default();
        let first = Arc::new(vec![0; 16 * 1024 * 1024]);
        let old = Arc::downgrade(&first);
        t.commit(0, first);
        for p in 1..5 {
            t.commit(p, Arc::new(vec![p as u8; 16 * 1024 * 1024]));
        }
        assert!(old.upgrade().is_none());
        assert!(t.bytes <= BYTES);
        t.commit(0, Arc::new(vec![0]));
        assert!(
            !t.at_tip(),
            "refetching an evicted ordered item must not move the frontier"
        );
        t.clear();
        assert!(t.get(0).is_none());
        assert_eq!(t.position(), None);
    }
    #[test]
    fn back_and_forward_replay_exact_images_without_losing_future() {
        let mut t = Timeline::default();
        t.commit(0, Arc::new(vec![1]));
        t.commit(1, Arc::new(vec![2]));
        assert_eq!(t.previous(), Some(0));
        t.commit(0, t.get(0).unwrap());
        assert!(!t.at_tip());
        assert_eq!(&**t.get(1).unwrap(), &[2]);
        assert_eq!(t.position(), Some(0));
    }
    #[test]
    fn bounded_history_does_not_offer_evicted_positions() {
        let mut t = Timeline::default();
        for p in 0..105 {
            t.commit(p, Arc::new(vec![p as u8]));
        }
        assert!(t.get(4).is_none());
        t.commit(5, t.get(5).unwrap());
        assert_eq!(t.previous(), None);
        assert_eq!(t.entries.len(), 100);
    }
}
