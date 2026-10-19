//! Explicit X11 desktop/window capture. Wayland screen access requires its portal.
use std::time::Instant;
use x11rb::{connection::Connection, protocol::{randr, xproto::{ConnectionExt, ImageFormat, ImageOrder, VisualClass}}, rust_connection::RustConnection};
use crate::{Capture, CaptureError, CaptureSource, CaptureTarget};
use uu_media::{frame::{PixelFormat, RawFrame}, Timestamp};

const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;
fn backend(error: impl std::fmt::Display) -> CaptureError { CaptureError::Backend(format!("X11: {error}")) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

fn connect(display: Option<&str>) -> Result<(RustConnection, usize), CaptureError> {
    if display.is_none() && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty()) {
        return Err(CaptureError::Backend("Wayland 桌面需要屏幕共享授权；不能用 Xwayland 根窗口代替桌面采集".into()));
    }
    x11rb::connect(display).map_err(backend)
}

pub fn monitors(display: Option<&str>) -> Result<Vec<Monitor>, CaptureError> {
    let (conn, screen) = connect(display)?;
    monitors_on(&conn, screen)
}

fn monitors_on(conn: &RustConnection, screen_index: usize) -> Result<Vec<Monitor>, CaptureError> {
    let screen = &conn.setup().roots[screen_index];
    let primary = randr::get_output_primary(conn, screen.root).ok().and_then(|c| c.reply().ok()).map(|r| r.output).unwrap_or(0);
    let mut monitors = Vec::new();
    if let Ok(cookie) = randr::get_screen_resources_current(conn, screen.root) {
        if let Ok(resources) = cookie.reply() {
            for output in resources.outputs {
                let Ok(cookie) = randr::get_output_info(conn, output, resources.config_timestamp) else { continue };
                let Ok(info) = cookie.reply() else { continue };
                if info.connection != randr::Connection::CONNECTED || info.crtc == 0 { continue; }
                let Ok(cookie) = randr::get_crtc_info(conn, info.crtc, resources.config_timestamp) else { continue };
                let Ok(crtc) = cookie.reply() else { continue };
                if crtc.width == 0 || crtc.height == 0 { continue; }
                monitors.push(Monitor { name: String::from_utf8_lossy(&info.name).into_owned(), x: crtc.x.into(), y: crtc.y.into(), width: crtc.width.into(), height: crtc.height.into(), primary: output == primary });
            }
        }
    }
    if monitors.is_empty() {
        monitors.push(Monitor { name: format!("X11 Screen {screen_index}"), x: 0, y: 0, width: screen.width_in_pixels.into(), height: screen.height_in_pixels.into(), primary: true });
    } else if !monitors.iter().any(|m| m.primary) {
        monitors[0].primary = true;
    }
    Ok(monitors)
}

pub struct X11Capture {
    conn: RustConnection,
    drawable: u32,
    area: Monitor,
    depth: u8,
    masks: [u32; 3],
    started: Instant,
}

impl X11Capture {
    pub fn open(display: Option<&str>, source: &CaptureSource) -> Result<Self, CaptureError> {
        let (conn, screen_index) = connect(display)?;
        let screen = &conn.setup().roots[screen_index];
        let (drawable, area, depth, visual) = match source {
            CaptureSource::Window(id) => {
                let window = u32::try_from(*id).map_err(backend)?;
                let geometry = conn.get_geometry(window).map_err(backend)?.reply().map_err(backend)?;
                let attributes = conn.get_window_attributes(window).map_err(backend)?.reply().map_err(backend)?;
                (window, Monitor { name: format!("Window {window}"), x: 0, y: 0, width: geometry.width.into(), height: geometry.height.into(), primary: false }, geometry.depth, attributes.visual)
            }
            CaptureSource::Primary | CaptureSource::Monitor(_) => {
                let list = monitors_on(&conn, screen_index)?;
                let selected = match source {
                    CaptureSource::Monitor(name) => list.into_iter().find(|m| &m.name == name).ok_or_else(|| CaptureError::TargetNotFound(CaptureTarget::Output(name.clone())))?,
                    _ => list.into_iter().find(|m| m.primary).ok_or(CaptureError::NoBackend)?,
                };
                (screen.root, selected, screen.root_depth, screen.root_visual)
            }
            _ => return Err(CaptureError::NoBackend),
        };
        if area.width == 0 || area.height == 0 || area.x < 0 || area.y < 0 {
            return Err(backend("invalid capture bounds"));
        }
        let visual = conn.setup().roots.iter().flat_map(|s| &s.allowed_depths)
            .flat_map(|d| &d.visuals).find(|v| v.visual_id == visual).ok_or_else(|| backend("visual unavailable"))?;
        if visual.class != VisualClass::TRUE_COLOR { return Err(backend("only TrueColor visuals are supported")); }
        let masks = [visual.red_mask, visual.green_mask, visual.blue_mask];
        Ok(Self { conn, drawable, area, depth, masks, started: Instant::now() })
    }
}

impl Capture for X11Capture {
    fn resolution(&self) -> (u32, u32) { (self.area.width, self.area.height) }
    fn next_frame(&mut self) -> Result<RawFrame, CaptureError> {
        let (width, height) = self.resolution();
        let format = self.conn.setup().pixmap_formats.iter().find(|f| f.depth == self.depth).ok_or_else(|| backend("pixel format unavailable"))?;
        let (bpp, pad, order) = (format.bits_per_pixel, format.scanline_pad, self.conn.setup().image_byte_order);
        let reply = self.conn.get_image(ImageFormat::Z_PIXMAP, self.drawable,
            i16::try_from(self.area.x).map_err(backend)?, i16::try_from(self.area.y).map_err(backend)?,
            u16::try_from(width).map_err(backend)?, u16::try_from(height).map_err(backend)?, u32::MAX)
            .map_err(backend)?.reply().map_err(backend)?;
        let data = unpack(&reply.data, width as usize, height as usize, bpp, pad, order, self.masks)?;
        Ok(RawFrame { width, height, format: PixelFormat::Bgra8, stride: width * 4, data: data.into(),
            pts: Timestamp(self.started.elapsed().as_micros().min(u64::MAX as u128) as u64), gpu: false })
    }
}

fn unpack(data: &[u8], width: usize, height: usize, bpp: u8, pad: u8, order: ImageOrder, masks: [u32; 3]) -> Result<Vec<u8>, CaptureError> {
    if !matches!(bpp, 16 | 24 | 32) || !matches!(pad, 8 | 16 | 32) || width == 0 || height == 0 {
        return Err(backend("unsupported pixel layout"));
    }
    let row_bits = width.checked_mul(bpp as usize).ok_or_else(|| backend("image too large"))?;
    let stride = row_bits.div_ceil(pad as usize).checked_mul(pad as usize / 8).ok_or_else(|| backend("image too large"))?;
    let needed = stride.checked_mul(height).filter(|n| *n <= MAX_FRAME_BYTES).ok_or_else(|| backend("image too large"))?;
    let output = width.checked_mul(height).and_then(|n| n.checked_mul(4)).filter(|n| *n <= MAX_FRAME_BYTES).ok_or_else(|| backend("image too large"))?;
    if data.len() < needed || masks.iter().any(|m| *m == 0) { return Err(backend("incomplete pixel buffer")); }
    let mut result = vec![0u8; output];
    if bpp == 32 && order == ImageOrder::LSB_FIRST && masks == [0xff0000, 0xff00, 0xff] {
        for y in 0..height {
            result[y * width * 4..(y + 1) * width * 4].copy_from_slice(&data[y * stride..y * stride + width * 4]);
        }
        for pixel in result.chunks_exact_mut(4) { pixel[3] = 255; }
        return Ok(result);
    }
    let bytes = bpp as usize / 8;
    for y in 0..height {
        for x in 0..width {
            let input = &data[y * stride + x * bytes..y * stride + (x + 1) * bytes];
            let value = if order == ImageOrder::LSB_FIRST { input.iter().enumerate().fold(0u32, |a, (i, b)| a | ((*b as u32) << (i * 8))) }
                else { input.iter().fold(0u32, |a, b| (a << 8) | *b as u32) };
            let channels = masks.map(|mask| { let shift = mask.trailing_zeros(); let max = mask >> shift; (((value & mask) >> shift) as u64 * 255 / max as u64) as u8 });
            let offset = (y * width + x) * 4;
            result[offset..offset + 4].copy_from_slice(&[channels[2], channels[1], channels[0], 255]);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xrgb_padding_is_made_opaque_and_rows_are_checked() {
        let data = [0x20, 0x40, 0xd0, 0, 0x20, 0x40, 0xd0, 0];
        let result = unpack(&data, 1, 2, 32, 32, ImageOrder::LSB_FIRST, [0xff0000, 0xff00, 0xff]).unwrap();
        assert_eq!(result, [0x20, 0x40, 0xd0, 255, 0x20, 0x40, 0xd0, 255]);
        assert!(unpack(&data[..7], 1, 2, 32, 32, ImageOrder::LSB_FIRST, [0xff0000, 0xff00, 0xff]).is_err());
    }
    #[test]
    fn padded_24bit_rows_and_big_endian_rgb565_are_decoded() {
        let result = unpack(&[0x20, 0x40, 0xd0, 99, 0, 0xff, 0, 99], 1, 2, 24, 32, ImageOrder::LSB_FIRST, [0xff0000, 0xff00, 0xff]).unwrap();
        assert_eq!(result, [0x20, 0x40, 0xd0, 255, 0, 255, 0, 255]);
        assert_eq!(unpack(&[0xf8, 0x00], 1, 1, 16, 16, ImageOrder::MSB_FIRST, [0xf800, 0x7e0, 0x1f]).unwrap(), [0, 0, 255, 255]);
    }
}
