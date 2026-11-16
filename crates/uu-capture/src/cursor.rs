//! Pointer metadata shares the capture desktop lifetime, never video pixels.
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};

pub const MAX_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    /// DXGI values: 1 = monochrome AND/XOR, 2 = BGRA, 4 = masked BGRA.
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub hotspot: [i32; 2],
}
impl Description {
    pub fn size(self) -> Option<[u32; 2]> {
        let h = if self.kind == 1 {
            if self.height % 2 != 0 {
                return None;
            }
            self.height / 2
        } else {
            self.height
        };
        ((1..=512).contains(&self.width) && (1..=512).contains(&h)).then_some([self.width, h])
    }
    pub fn bytes(self) -> Option<usize> {
        let [w, h] = self.size()?;
        let row = match self.kind {
            1 => w.div_ceil(8),
            2 | 4 => w * 4,
            _ => return None,
        };
        if self.pitch < row
            || self.hotspot[0] < 0
            || self.hotspot[1] < 0
            || self.hotspot[0] >= w as i32
            || self.hotspot[1] >= h as i32
        {
            return None;
        }
        let len = (self.pitch as usize).checked_mul(self.height as usize)?;
        (len <= MAX_BYTES).then_some(len)
    }
}
#[derive(Clone, Debug)]
pub struct Shape {
    pub description: Description,
    pub data: Vec<u8>,
}
impl Shape {
    pub fn valid(&self) -> bool {
        self.description.bytes() == Some(self.data.len())
    }
    /// Straight RGBA plus an RGB XOR operand; only cursor pixels are converted.
    pub fn layers(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        if !self.valid() {
            return None;
        }
        let d = self.description;
        let [w, h] = d.size()?;
        let mut alpha = vec![0; w as usize * h as usize * 4];
        let mut xor = alpha.clone();
        for y in 0..h as usize {
            for x in 0..w as usize {
                let to = (y * w as usize + x) * 4;
                if d.kind == 1 {
                    let at = y * d.pitch as usize + x / 8;
                    let mask = 0x80 >> (x % 8);
                    let and = self.data[at] & mask != 0;
                    let invert = self.data[at + d.pitch as usize * h as usize] & mask != 0;
                    match (and, invert) {
                        (false, false) => alpha[to..to + 4].copy_from_slice(&[0, 0, 0, 255]),
                        (false, true) => alpha[to..to + 4].copy_from_slice(&[255; 4]),
                        (true, true) => xor[to..to + 4].copy_from_slice(&[255; 4]),
                        _ => {}
                    }
                } else {
                    let at = y * d.pitch as usize + x * 4;
                    let p = &self.data[at..at + 4];
                    if d.kind == 4 && p[3] == 255 {
                        xor[to..to + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
                    } else {
                        alpha[to..to + 4].copy_from_slice(&[
                            p[2],
                            p[1],
                            p[0],
                            if d.kind == 4 { 255 } else { p[3] },
                        ]);
                    }
                }
            }
        }
        Some((alpha, xor))
    }
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub revision: u64,
    pub shape_id: u64,
    /// Top-left in physical output pixels, already independent of the hotspot.
    pub position: Option<[i32; 2]>,
    pub desktop: [u32; 2],
    pub shape: Option<Arc<Shape>>,
}
#[derive(Clone, Default)]
pub struct Stream(Arc<Shared>);
type Wake = dyn Fn() + Send + Sync;
#[derive(Default)]
struct Shared {
    state: Mutex<Snapshot>,
    subscribers: Mutex<Vec<Weak<Wake>>>,
}
/// Owns one callback without retaining the capture stream. A notification
/// already collected before removal may finish; callbacks run outside locks.
#[must_use = "Keep the subscription alive to receive cursor updates"]
pub struct Subscription {
    stream: Weak<Shared>,
    wake: Arc<Wake>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.upgrade() {
            let own = Arc::downgrade(&self.wake);
            stream.subscribers.lock().unwrap().retain(|wake| !Weak::ptr_eq(wake, &own));
        }
    }
}
impl Stream {
    pub fn snapshot(&self) -> Snapshot {
        self.0.state.lock().unwrap().clone()
    }
    pub fn subscribe(&self, wake: impl Fn() + Send + Sync + 'static) -> Subscription {
        let wake: Arc<Wake> = Arc::new(wake);
        self.0.subscribers.lock().unwrap().push(Arc::downgrade(&wake));
        Subscription { stream: Arc::downgrade(&self.0), wake }
    }
    fn notify(&self) {
        let wakes: Vec<_> = self.0.subscribers.lock().unwrap().iter().filter_map(Weak::upgrade).collect();
        for wake in wakes {
            wake();
        }
    }
    pub fn update(&self, desktop: [u32; 2], position: Option<[i32; 2]>, shape: Option<Shape>) {
        let mut state = self.0.state.lock().unwrap();
        if state.desktop == desktop && state.position == position && shape.is_none() {
            return;
        }
        if let Some(shape) = shape {
            if !shape.valid() {
                return;
            }
            state.shape_id = state.shape_id.wrapping_add(1).max(1);
            state.shape = Some(Arc::new(shape));
        }
        state.desktop = desktop;
        state.position = position;
        state.revision = state.revision.wrapping_add(1).max(1);
        drop(state);
        self.notify();
    }
    pub fn clear(&self) {
        let mut s = self.0.state.lock().unwrap();
        if s.position.is_none() && s.shape.is_none() {
            return;
        }
        s.revision = s.revision.wrapping_add(1).max(1);
        s.shape_id = s.shape_id.wrapping_add(1).max(1);
        s.position = None;
        s.shape = None;
        drop(s);
        self.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_subscriber_wakes_and_dropping_one_preserves_the_other() {
        let stream = Stream::default();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let count = first.clone();
        let a = stream.subscribe(move || { count.fetch_add(1, Ordering::Relaxed); });
        let count = second.clone();
        let b = stream.subscribe(move || { count.fetch_add(1, Ordering::Relaxed); });
        stream.update([100, 100], Some([10, 20]), None);
        assert_eq!(first.load(Ordering::Relaxed), 1);
        assert_eq!(second.load(Ordering::Relaxed), 1);
        drop(a);
        stream.update([100, 100], Some([20, 20]), None);
        assert_eq!(first.load(Ordering::Relaxed), 1);
        assert_eq!(second.load(Ordering::Relaxed), 2);
        assert_eq!(stream.0.subscribers.lock().unwrap().len(), 1);
        drop(b);
        assert!(stream.0.subscribers.lock().unwrap().is_empty());
    }

    #[test]
    fn callback_can_unregister_itself_and_reenter_the_stream_without_holding_locks() {
        let stream = Stream::default();
        let own = Arc::new(Mutex::new(None::<Subscription>));
        let remove = own.clone();
        let weak = Arc::downgrade(&stream.0);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        *own.lock().unwrap() = Some(stream.subscribe(move || {
            let stream = Stream(weak.upgrade().unwrap());
            assert_eq!(stream.snapshot().position, Some([10, 20]));
            count.fetch_add(1, Ordering::Relaxed);
            let subscription = remove.lock().unwrap().take();
            drop(subscription);
            stream.update([100, 100], Some([20, 20]), None);
        }));
        let other = Arc::new(AtomicUsize::new(0));
        let count = other.clone();
        let _other = stream.subscribe(move || { count.fetch_add(1, Ordering::Relaxed); });
        let target = stream.clone();
        let (done, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            target.update([100, 100], Some([10, 20]), None);
            done.send(()).unwrap();
        });
        receive.recv_timeout(std::time::Duration::from_secs(1)).expect("reentrant notification deadlocked");
        thread.join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(other.load(Ordering::Relaxed), 2, "both real updates must reach the remaining subscriber");
        assert!(own.lock().unwrap().is_none());
    }

    #[test]
    fn a_subscription_does_not_retain_the_stream_it_weakly_observes() {
        let stream = Stream::default();
        let weak = Arc::downgrade(&stream.0);
        let observer = weak.clone();
        let subscription = stream.subscribe(move || { let _ = observer.upgrade(); });
        drop(stream);
        assert!(weak.upgrade().is_none());
        drop(subscription);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn dropping_a_callback_that_captures_the_stream_creates_no_registry_cycle() {
        let stream = Stream::default();
        let weak = Arc::downgrade(&stream.0);
        let captured = stream.clone();
        let subscription = stream.subscribe(move || { let _ = captured.snapshot(); });
        drop(stream);
        assert!(weak.upgrade().is_some(), "the caller's callback still owns its captured stream");
        drop(subscription);
        assert!(weak.upgrade().is_none(), "the weak callback registry must not keep its own stream alive");
    }

    #[test]
    fn mono_and_xor_masks_padded_rows_and_hidden_lifecycle() {
        let shape = Shape {
            description: Description {
                kind: 1,
                width: 4,
                height: 2,
                pitch: 2,
                hotspot: [1, 0],
            },
            data: vec![0b00110000, 77, 0b01010000, 88],
        };
        let (a, x) = shape.layers().unwrap();
        assert_eq!(
            a,
            vec![0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(&x[12..], &[255; 4]);
        assert!(x[..12].iter().all(|v| *v == 0));
        let stream = Stream::default();
        stream.update([1920, 1080], Some([-1, 20]), Some(shape));
        let s = stream.snapshot();
        stream.update([1920, 1080], Some([-1, 20]), None);
        assert_eq!(s.revision, stream.snapshot().revision);
        stream.clear();
        let hidden = stream.snapshot();
        assert!(
            hidden.revision > s.revision && hidden.shape.is_none() && hidden.position.is_none()
        );
        assert!(Description {
            pitch: u32::MAX,
            ..s.shape.unwrap().description
        }
        .bytes()
        .is_none());
    }
    #[test]
    fn color_alpha_and_masked_color_are_distinct() {
        let mut shape = Shape {
            description: Description {
                kind: 2,
                width: 2,
                height: 1,
                pitch: 8,
                hotspot: [0, 0],
            },
            data: vec![1, 2, 3, 0, 4, 5, 6, 255],
        };
        assert_eq!(shape.layers().unwrap().0, vec![3, 2, 1, 0, 6, 5, 4, 255]);
        shape.description.kind = 4;
        let (a, x) = shape.layers().unwrap();
        assert_eq!(a, vec![3, 2, 1, 255, 0, 0, 0, 0]);
        assert_eq!(&x[4..], &[6, 5, 4, 255]);
    }
}
