//! Recycle owned CPU readback storage after the last Bytes reference disappears.
//! No mapped GPU pointer or driver object crosses threads.
use std::sync::{Arc, Mutex};
const MAX_BUFFERS: usize = 2;
const MAX_BUFFER_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Default)]
pub(super) struct Pool(Arc<Mutex<Vec<Vec<u8>>>>);
impl Pool {
    pub fn take(&self, size: usize) -> Pixels {
        let mut buffers = self.0.lock().unwrap();
        let chosen = buffers.iter().position(|b| b.capacity() >= size);
        let mut bytes = chosen.map(|i| buffers.swap_remove(i)).unwrap_or_default();
        drop(buffers);
        // Existing equal-sized buffers retain initialized storage. The crop
        // operation overwrites every byte before publishing the frame.
        bytes.resize(size, 0);
        Pixels {
            bytes,
            pool: self.clone(),
        }
    }
}
pub(super) struct Pixels {
    bytes: Vec<u8>,
    pool: Pool,
}
impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl AsMut<[u8]> for Pixels {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}
impl Drop for Pixels {
    fn drop(&mut self) {
        if self.bytes.capacity() > MAX_BUFFER_BYTES {
            return;
        }
        if let Ok(mut buffers) = self.pool.0.lock() {
            if buffers.len() < MAX_BUFFERS {
                buffers.push(std::mem::take(&mut self.bytes));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reused_pixels_do_not_allocate_or_clear_initialized_storage() {
        let pool = Pool::default();
        let mut frame = pool.take(24);
        frame.as_mut().fill(77);
        let pointer = frame.as_ref().as_ptr();
        drop(frame);
        let recycled = pool.take(24);
        assert_eq!(recycled.as_ref().as_ptr(), pointer);
        assert_eq!(recycled.as_ref(), &[77; 24]);
    }
    #[test]
    fn cloned_bytes_keep_their_pixels_until_the_last_reader_drops() {
        let pool = Pool::default();
        let mut frame = pool.take(24);
        frame.as_mut().fill(42);
        let first = bytes::Bytes::from_owner(frame);
        let pointer = first.as_ptr();
        let reader = first.clone();
        drop(first);
        let mut next = pool.take(24);
        assert_ne!(next.as_ref().as_ptr(), pointer);
        next.as_mut().fill(99);
        assert_eq!(reader.as_ref(), &[42; 24]);
        drop(reader);
        let recycled = pool.take(24);
        assert_eq!(recycled.as_ref().as_ptr(), pointer);
        assert_eq!(recycled.as_ref(), &[42; 24]);
    }
    #[test]
    fn a_burst_of_live_frames_does_not_grow_the_retained_pool() {
        let pool = Pool::default();
        let frames: Vec<_> = (0..10).map(|_| pool.take(128)).collect();
        drop(frames);
        assert_eq!(pool.0.lock().unwrap().len(), MAX_BUFFERS);
    }
}
