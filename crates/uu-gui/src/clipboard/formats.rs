//! All codecs are bounded before network images reach the system clipboard.
#[cfg(any(windows, test))]
use image::ImageDecoder;
use image::ImageEncoder;
use std::io::{Cursor, Write};
use uu_core::clipboard::{Content, Image, MAX_IMAGE_BYTES, MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS};
#[derive(Clone)]
pub(super) struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
impl Pixels {
    pub fn validate(&self) -> anyhow::Result<()> {
        Image::dimensions(self.width, self.height)?;
        anyhow::ensure!(
            self.rgba.len() == self.width as usize * self.height as usize * 4,
            "图片像素缓冲不完整"
        );
        Ok(())
    }
    pub fn fingerprint(&self) -> [u8; 32] {
        use sha2::Digest;
        let mut hash = sha2::Sha256::new();
        hash.update(b"image");
        hash.update(self.width.to_le_bytes());
        hash.update(self.height.to_le_bytes());
        hash.update(&self.rgba);
        hash.finalize().into()
    }
    pub fn encode(&self) -> anyhow::Result<Image> {
        self.validate()?;
        struct Bounded(Vec<u8>);
        impl Write for Bounded {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self
                    .0
                    .len()
                    .checked_add(bytes.len())
                    .is_none_or(|len| len > MAX_IMAGE_BYTES)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        "剪贴板图片超过8MiB",
                    ));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut png = Bounded(Vec::new());
        image::codecs::png::PngEncoder::new_with_quality(
            &mut png,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(
            &self.rgba,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )?;
        Image::new(self.width, self.height, png.0)
    }
}
#[derive(Clone)]
pub(super) enum Value {
    Text(String),
    Image(Pixels),
}
impl Value {
    pub fn fingerprint(&self) -> [u8; 32] {
        match self {
            Self::Text(text) => {
                use sha2::Digest;
                let mut hash = sha2::Sha256::new();
                hash.update(b"text");
                hash.update(super::canonical(text));
                hash.finalize().into()
            }
            Self::Image(image) => image.fingerprint(),
        }
    }
    pub fn encode(&self) -> anyhow::Result<Content> {
        match self {
            Self::Text(text) => Ok(Content::Text(text.clone())),
            Self::Image(image) => Ok(Content::Image(image.encode()?)),
        }
    }
    pub fn decode(content: &Content) -> anyhow::Result<Self> {
        match content {
            Content::Text(text) => Ok(Self::Text(text.clone())),
            Content::Image(image) => {
                // Header dimensions have already been checked by the wire codec.
                Image::new(image.width, image.height, image.png.clone())?;
                let mut reader = image::ImageReader::with_format(
                    Cursor::new(&*image.png),
                    image::ImageFormat::Png,
                );
                reader.limits(limits());
                let decoded = reader.decode()?.into_rgba8();
                let (width, height) = decoded.dimensions();
                anyhow::ensure!(
                    (width, height) == (image.width, image.height),
                    "PNG解码尺寸不一致"
                );
                Ok(Self::Image(Pixels {
                    width,
                    height,
                    rgba: decoded.into_raw(),
                }))
            }
        }
    }
}
fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_PIXELS * 4);
    limits
}
#[cfg(any(windows, test))]
pub(super) fn dib_decode(data: &[u8]) -> anyhow::Result<Pixels> {
    anyhow::ensure!(
        data.len() >= 40 && data.len() <= MAX_IMAGE_PIXELS as usize * 4 + 4096,
        "DIB数据超限或不完整"
    );
    let header = u32::from_le_bytes(data[..4].try_into()?);
    let bits = u16::from_le_bytes(data[14..16].try_into()?);
    let compression = u32::from_le_bytes(data[16..20].try_into()?);
    anyhow::ensure!(
        matches!(header, 40 | 108 | 124)
            && u16::from_le_bytes(data[12..14].try_into()?) == 1
            && matches!(bits, 24 | 32)
            && matches!(compression, 0 | 3 | 6)
            && u32::from_le_bytes(data[32..36].try_into()?) == 0,
        "不支持的DIB像素格式"
    );
    let width = i32::from_le_bytes(data[4..8].try_into()?);
    let height = i32::from_le_bytes(data[8..12].try_into()?);
    anyhow::ensure!(
        width > 0 && height != 0 && height != i32::MIN,
        "DIB尺寸无效"
    );
    Image::dimensions(width as u32, height.unsigned_abs())?;
    let mut data = data.to_vec();
    // Some producers declare alpha masks but leave BI_RGB in a V5 header.
    // Honour the explicit standard masks instead of discarding that alpha.
    if data.len() >= 124
        && u32::from_le_bytes(data[..4].try_into()?) >= 124
        && u16::from_le_bytes(data[14..16].try_into()?) == 32
        && u32::from_le_bytes(data[16..20].try_into()?) == 0
        && data[40..56] == [0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 255]
    {
        data[16..20].copy_from_slice(&3u32.to_le_bytes());
    }
    // Give the mature BMP decoder an explicit pixel offset. V4/V5 masks live
    // inside their header; only a 40-byte BITMAPINFOHEADER has external masks.
    // An explicit file header also avoids depending on inferred DIB offsets.
    let offset = header as usize
        + if header == 40 && compression == 3 {
            12
        } else {
            0
        };
    let stride = (u64::from(width as u32) * u64::from(bits)).div_ceil(32) * 4;
    anyhow::ensure!(
        offset <= data.len()
            && stride * u64::from(height.unsigned_abs())
                <= data.len().saturating_sub(offset) as u64,
        "DIB像素数据不完整"
    );
    let mut bitmap = b"BM".to_vec();
    bitmap.extend_from_slice(&((data.len() + 14) as u32).to_le_bytes());
    bitmap.extend_from_slice(&0u32.to_le_bytes());
    bitmap.extend_from_slice(&((offset + 14) as u32).to_le_bytes());
    bitmap.extend(data);
    let mut decoder = image::codecs::bmp::BmpDecoder::new(Cursor::new(bitmap))?;
    decoder.set_limits(limits())?;
    let decoded = image::DynamicImage::from_decoder(decoder)?.into_rgba8();
    let (width, height) = decoded.dimensions();
    let image = Pixels {
        width,
        height,
        rgba: decoded.into_raw(),
    };
    image.validate()?;
    Ok(image)
}
#[cfg(any(windows, test))]
pub(super) fn dib_encode(pixels: &Pixels) -> anyhow::Result<Vec<u8>> {
    pixels.validate()?;
    let mut dib = vec![0u8; 124 + pixels.rgba.len()];
    for (offset, value) in [
        (0, 124u32),
        (4, pixels.width),
        (8, (pixels.height as i32).wrapping_neg() as u32),
        (16, 3),
        (20, pixels.rgba.len() as u32),
        (40, 0x00ff0000),
        (44, 0x0000ff00),
        (48, 0x000000ff),
        (52, 0xff000000),
        (56, 0x73524742),
        (108, 4),
    ] {
        dib[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    dib[12..14].copy_from_slice(&1u16.to_le_bytes());
    dib[14..16].copy_from_slice(&32u16.to_le_bytes());
    for (src, dst) in pixels
        .rgba
        .chunks_exact(4)
        .zip(dib[124..].chunks_exact_mut(4))
    {
        dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
    }
    Ok(dib)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn png_and_top_down_dib_preserve_rgba_pixels_and_alpha() {
        let pixels = Pixels {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 255, 255, 255, 255,
            ],
        };
        let content = Content::Image(pixels.encode().unwrap());
        let Value::Image(decoded) = Value::decode(&content).unwrap() else {
            panic!("image required")
        };
        assert_eq!(decoded.rgba, pixels.rgba);
        assert_eq!(decoded.fingerprint(), pixels.fingerprint());
        let dib = dib_encode(&pixels).unwrap();
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), -2);
        let decoded = dib_decode(&dib).unwrap();
        assert_eq!(decoded.rgba, pixels.rgba);
    }
    #[test]
    fn bottom_up_24_bit_dib_handles_row_padding_without_swapping_red_and_blue() {
        let mut dib = vec![0u8; 40 + 16];
        dib[..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&2i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib[40..].copy_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 0]);
        assert_eq!(
            dib_decode(&dib).unwrap().rgba,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]
        );
    }
    #[test]
    fn malformed_or_oversized_image_never_decodes_into_an_os_value() {
        assert!(Image::dimensions(8193, 1).is_err());
        assert!(Image::dimensions(8192, 8192).is_err());
        let pixels = Pixels {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        };
        let valid = pixels.encode().unwrap();
        let mut bytes = valid.png.to_vec();
        bytes[16..20].copy_from_slice(&8193u32.to_be_bytes());
        assert!(Image::new(1, 1, bytes).is_err());
        let mut bytes = valid.png.to_vec();
        let at = bytes.len() / 2;
        bytes[at] ^= 255;
        let bad = Image::new(1, 1, bytes).unwrap();
        assert!(Value::decode(&Content::Image(bad)).is_err());
        assert!(dib_decode(&[0u8; 39]).is_err());
        let mut dib = dib_encode(&pixels).unwrap();
        dib[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(dib_decode(&dib).is_err());
    }
}
