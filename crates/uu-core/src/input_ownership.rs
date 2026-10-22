//! One keyboard/mouse owner across all sessions of a running native host.
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State { owner: Option<u64>, failed_release: bool, next: u64 }
#[derive(Default)]
pub struct Authority(Mutex<State>);
impl Authority {
    pub(crate) fn claim(self: &Arc<Self>) -> Option<Lease> {
        let mut state = self.0.lock().unwrap();
        if state.owner.is_some() || state.failed_release { return None; }
        state.next = state.next.wrapping_add(1);
        let id = state.next;
        state.owner = Some(id);
        Some(Lease { authority: self.clone(), id })
    }
}
pub(crate) struct Lease { authority: Arc<Authority>, id: u64 }
impl Lease {
    /// A failed OS release cannot safely hand held keys to another session.
    pub(crate) fn poison(&self) { self.authority.0.lock().unwrap().failed_release = true; }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.authority.0.lock().unwrap();
        if state.owner == Some(self.id) { state.owner = None; }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_handover_requires_cleanup_and_failed_release_blocks_new_control() {
        let authority = Arc::new(Authority::default());
        let first = authority.claim().unwrap();
        assert!(authority.claim().is_none());
        drop(first);
        let second = authority.claim().unwrap();
        second.poison();
        drop(second);
        assert!(authority.claim().is_none());
        assert!(Arc::new(Authority::default()).claim().is_some(), "different hosts do not share input");
    }
}
