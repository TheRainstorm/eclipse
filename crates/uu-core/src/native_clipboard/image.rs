//! Ordered, bounded PNG chunks; partial images never replace an OS clipboard.
use crate::clipboard::{Image, MAX_IMAGE_BYTES};
use std::time::{Duration, Instant};
pub(super) const CHUNK: usize = 8 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);
#[derive(Clone, Copy)]
pub(super) struct Chunk<'a> {
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub total: u32,
    pub offset: u32,
    pub bytes: &'a [u8],
}
impl Chunk<'_> {
    pub fn valid(&self) -> bool {
        self.revision > 0
            && Image::dimensions(self.width, self.height).is_ok()
            && self.total > 0
            && self.total as usize <= MAX_IMAGE_BYTES
            && !self.bytes.is_empty()
            && self.bytes.len() <= CHUNK
            && (self.offset as usize)
                .checked_add(self.bytes.len())
                .is_some_and(|end| end <= self.total as usize)
    }
}
struct Partial {
    revision: u64,
    width: u32,
    height: u32,
    total: u32,
    bytes: Vec<u8>,
    started: Instant,
}
#[derive(Default)]
pub(super) struct Assembly {
    partial: Option<Partial>,
}
impl Assembly {
    pub fn clear(&mut self) {
        self.partial = None;
    }
    pub fn push(&mut self, chunk: Chunk<'_>, now: Instant) -> anyhow::Result<Option<Image>> {
        anyhow::ensure!(chunk.valid(), "{}", uu_i18n::format!("剪贴板图片分片无效"));
        if chunk.offset == 0 {
            self.partial = Some(Partial {
                revision: chunk.revision,
                width: chunk.width,
                height: chunk.height,
                total: chunk.total,
                bytes: Vec::with_capacity(chunk.total as usize),
                started: now,
            });
        }
        let Some(partial) = &mut self.partial else {
            return Ok(None);
        };
        if now.duration_since(partial.started) > DEADLINE {
            self.clear();
            return Ok(None);
        }
        anyhow::ensure!(
            partial.revision == chunk.revision
                && partial.width == chunk.width
                && partial.height == chunk.height
                && partial.total == chunk.total
                && partial.bytes.len() == chunk.offset as usize,
            "{}", uu_i18n::format!("剪贴板图片分片次序不一致"));
        partial.bytes.extend_from_slice(chunk.bytes);
        if partial.bytes.len() != partial.total as usize {
            return Ok(None);
        }
        let partial = self.partial.take().unwrap();
        Ok(Some(Image::new(
            partial.width,
            partial.height,
            partial.bytes,
        )?))
    }
}
pub(super) struct Sending {
    pub image: Image,
    pub revision: u64,
    pub offset: usize,
    pub started: Instant,
    pub epoch: u64,
}
impl Sending {
    pub fn chunk(&self) -> Chunk<'_> {
        Chunk {
            revision: self.revision,
            width: self.image.width,
            height: self.image.height,
            total: self.image.png.len() as u32,
            offset: self.offset as u32,
            bytes: &self.image.png[self.offset..(self.offset + CHUNK).min(self.image.png.len())],
        }
    }
    pub fn expired(&self) -> bool {
        self.started.elapsed() > DEADLINE
    }
}
