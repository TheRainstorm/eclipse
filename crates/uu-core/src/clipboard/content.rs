//! Clipboard content is one latest value, never independent text/image queues.
use anyhow::{ensure, Result};
use std::sync::Arc;
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 8192;
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub png: Arc<[u8]>,
}
impl Image {
    pub fn dimensions(width: u32, height: u32) -> Result<()> {
        ensure!(
            width > 0
                && height > 0
                && width <= MAX_IMAGE_DIMENSION
                && height <= MAX_IMAGE_DIMENSION
                && u64::from(width) * u64::from(height) <= MAX_IMAGE_PIXELS,
            "剪贴板图片尺寸超限（最大8192边长、16M像素）"
        );
        Ok(())
    }
    pub fn new(width: u32, height: u32, png: impl Into<Arc<[u8]>>) -> Result<Self> {
        Self::dimensions(width, height)?;
        let png = png.into();
        ensure!(
            png.len() >= 33 && png.len() <= MAX_IMAGE_BYTES,
            "剪贴板PNG超出8MiB限制或不完整"
        );
        ensure!(
            &png[..8] == b"\x89PNG\r\n\x1a\n"
                && &png[8..12] == 13u32.to_be_bytes().as_slice()
                && &png[12..16] == b"IHDR",
            "剪贴板图片不是PNG"
        );
        ensure!(
            u32::from_be_bytes(png[16..20].try_into().unwrap()) == width
                && u32::from_be_bytes(png[20..24].try_into().unwrap()) == height,
            "剪贴板PNG尺寸与声明不符"
        );
        Ok(Self { width, height, png })
    }
}
#[derive(Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Image(Image),
}
impl Content {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}
impl From<String> for Content {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}
impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Self::Text(text.into())
    }
}
impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => f.debug_struct("Text").field("bytes", &text.len()).finish(),
            Self::Image(image) => f
                .debug_struct("Image")
                .field("width", &image.width)
                .field("height", &image.height)
                .field("bytes", &image.png.len())
                .finish(),
        }
    }
}
