//! Pointer metadata shares the capture desktop lifetime, never video pixels.
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

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
#[derive(Default)]
struct Shared {
    state: Mutex<Snapshot>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl Stream {
    pub fn snapshot(&self) -> Snapshot {
        self.0.state.lock().unwrap().clone()
    }
    pub fn set_waker(&self, wake: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.0.wake.lock().unwrap() = wake;
    }
    fn notify(&self) {
        let wake = self.0.wake.lock().unwrap().clone();
        if let Some(wake) = wake {
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
