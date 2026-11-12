//! Fresh SDR reads for a valid desktop whose duplication has no initial image.
//! Handles stay within each read, including when a Capture moves between threads.
use super::{backend, Monitor};
use crate::{
    cursor::{Description, Shape, Stream},
    CaptureError,
};
use uu_media::{
    frame::{PixelFormat, RawFrame},
    Timestamp,
};
use windows::core::BOOL;
use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};

struct Dc {
    handle: HDC,
    screen: bool,
}
impl Drop for Dc {
    fn drop(&mut self) {
        unsafe {
            if self.screen {
                let _ = ReleaseDC(None, self.handle);
            } else {
                let _ = DeleteDC(self.handle);
            }
        }
    }
}
struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe {
                let _ = DeleteObject(self.0.into());
            }
        }
    }
}
struct Selection {
    dc: HDC,
    previous: HGDIOBJ,
}
impl Drop for Selection {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
        }
    }
}

pub(super) fn snapshot(
    source: &Monitor,
    interface: &str,
    pts: Timestamp,
    cursor: &Stream,
) -> Result<RawFrame, CaptureError> {
    let _coordinates = uu_input::windows_desktop::PhysicalCoordinates::enter().map_err(backend)?;
    if uu_input::windows_desktop::sync_input_desktop().map_err(backend)? {
        return Err(CaptureError::Suspended(
            "desktop changed during CPU capture".into(),
        ));
    }
    // A source name can be reused after hotplug. Never read another monitor's
    // rectangle or silently keep the old input mapping after a layout change.
    if super::monitor_interface(&source.name)? != interface {
        return Err(CaptureError::Suspended(
            "monitor changed during CPU capture".into(),
        ));
    }
    let current = monitor(&source.name)?;
    if (current.x, current.y, current.width, current.height)
        != (source.x, source.y, source.width, source.height)
    {
        return Err(CaptureError::Suspended(
            "monitor geometry changed during CPU capture".into(),
        ));
    }
    let width = i32::try_from(source.width).map_err(backend)?;
    let height = i32::try_from(source.height).map_err(backend)?;
    let length = (source.width as usize)
        .checked_mul(source.height as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n > 0 && *n <= 256 * 1024 * 1024)
        .ok_or_else(|| backend("CPU desktop frame too large"))?;
    let screen = Dc {
        handle: unsafe { GetDC(None) },
        screen: true,
    };
    if screen.handle.0.is_null() {
        return Err(backend("cannot read current desktop DC"));
    }
    let memory = Dc {
        handle: unsafe { CreateCompatibleDC(Some(screen.handle)) },
        screen: false,
    };
    if memory.handle.0.is_null() {
        return Err(backend("cannot create desktop readback DC"));
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = std::ptr::null_mut();
    let bitmap = Bitmap(
        unsafe {
            CreateDIBSection(
                Some(screen.handle),
                &info,
                DIB_RGB_COLORS,
                &mut pixels,
                None,
                0,
            )
        }
        .map_err(backend)?,
    );
    if pixels.is_null() {
        return Err(backend("desktop readback has no pixels"));
    }
    let previous = unsafe { SelectObject(memory.handle, bitmap.0.into()) };
    if previous.0.is_null() || previous.0 as isize == -1 {
        return Err(backend("cannot select desktop readback bitmap"));
    }
    let _selection = Selection {
        dc: memory.handle,
        previous,
    };
    unsafe {
        BitBlt(
            memory.handle,
            0,
            0,
            width,
            height,
            Some(screen.handle),
            source.x,
            source.y,
            SRCCOPY | CAPTUREBLT,
        )
        .map_err(backend)?;
        if !GdiFlush().as_bool() {
            return Err(backend("desktop readback did not complete"));
        }
    }
    let data = unsafe { std::slice::from_raw_parts(pixels.cast::<u8>(), length) }.to_vec();
    if uu_input::windows_desktop::input_desktop_changed().map_err(backend)? {
        return Err(CaptureError::Suspended(
            "desktop changed before CPU image publication".into(),
        ));
    }
    if let Some((position, shape)) = pointer(screen.handle, source) {
        cursor.update([source.width, source.height], position, shape);
    } else {
        cursor.clear();
    }
    Ok(RawFrame {
        width: source.width,
        height: source.height,
        stride: source.width * 4,
        format: PixelFormat::Bgra8,
        data: data.into(),
        pts,
        gpu: false,
        dma_buf: None,
        d3d11: None,
    })
}

fn monitor(name: &str) -> Result<Monitor, CaptureError> {
    struct Search<'a> {
        name: &'a str,
        found: Option<Monitor>,
    }
    unsafe extern "system" fn visit(handle: HMONITOR, _: HDC, _: *mut RECT, param: LPARAM) -> BOOL {
        let search = &mut *(param.0 as *mut Search<'_>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of_val(&info) as u32;
        if GetMonitorInfoW(handle, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            let end = info
                .szDevice
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(info.szDevice.len());
            let device = String::from_utf16_lossy(&info.szDevice[..end]);
            if device.eq_ignore_ascii_case(search.name) {
                let r = info.monitorInfo.rcMonitor;
                search.found = Some(Monitor {
                    name: device,
                    x: r.left,
                    y: r.top,
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                });
            }
        }
        true.into()
    }
    let mut search = Search { name, found: None };
    unsafe {
        if !EnumDisplayMonitors(
            None,
            None,
            Some(visit),
            LPARAM((&mut search as *mut Search<'_>) as isize),
        )
        .as_bool()
        {
            return Err(backend("cannot verify CPU capture monitor"));
        }
    }
    search
        .found
        .ok_or_else(|| CaptureError::Suspended("CPU capture monitor is not active".into()))
}

// GDI does not include the hardware cursor. Keep it in the same metadata stream
// as DXGI so each viewer's Local/Remote/Both preference remains independent.
fn pointer(dc: HDC, source: &Monitor) -> Option<(Option<[i32; 2]>, Option<Shape>)> {
    let mut cursor = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetCursorInfo(&mut cursor).ok()?;
    }
    if cursor.flags != CURSOR_SHOWING {
        return Some((None, None));
    }
    let x = i64::from(cursor.ptScreenPos.x) - i64::from(source.x);
    let y = i64::from(cursor.ptScreenPos.y) - i64::from(source.y);
    if !(0..i64::from(source.width)).contains(&x) || !(0..i64::from(source.height)).contains(&y) {
        return Some((None, None));
    }
    let mut icon = ICONINFO::default();
    unsafe {
        GetIconInfo(HICON(cursor.hCursor.0), &mut icon).ok()?;
    }
    let color = Bitmap(icon.hbmColor);
    let mask = Bitmap(icon.hbmMask);
    let source_bitmap = if color.0 .0.is_null() {
        mask.0
    } else {
        color.0
    };
    let mut info = BITMAP::default();
    if unsafe {
        GetObjectW(
            source_bitmap.into(),
            std::mem::size_of_val(&info) as i32,
            Some((&mut info as *mut BITMAP).cast()),
        )
    } == 0
    {
        return None;
    }
    let (width, height) = (
        u32::try_from(info.bmWidth).ok()?,
        u32::try_from(info.bmHeight).ok()?,
    );
    let monochrome = color.0 .0.is_null();
    let visual_height = if monochrome { height / 2 } else { height };
    if !(1..=512).contains(&width) || !(1..=512).contains(&visual_height) {
        return None;
    }
    let hotspot = [
        i32::try_from(icon.xHotspot).ok()?,
        i32::try_from(icon.yHotspot).ok()?,
    ];
    let (kind, pitch, data) = if monochrome {
        let pitch = width.div_ceil(32) * 4;
        (1, pitch, bitmap_bits(dc, mask.0, width, height, 1, pitch)?)
    } else {
        let pitch = width * 4;
        let mut pixels = bitmap_bits(dc, color.0, width, height, 32, pitch)?;
        let kind = if pixels.chunks_exact(4).any(|p| p[3] != 0) {
            2
        } else {
            let mask_pitch = width.div_ceil(32) * 4;
            let bits = bitmap_bits(dc, mask.0, width, height, 1, mask_pitch)?;
            for y in 0..height as usize {
                for x in 0..width as usize {
                    pixels[y * pitch as usize + x * 4 + 3] =
                        if bits[y * mask_pitch as usize + x / 8] & (0x80 >> (x % 8)) != 0 {
                            255
                        } else {
                            0
                        };
                }
            }
            4
        };
        (kind, pitch, pixels)
    };
    let shape = Shape {
        description: Description {
            kind,
            width,
            height,
            pitch,
            hotspot,
        },
        data,
    };
    if !shape.valid() {
        return None;
    }
    Some((
        Some([x as i32 - hotspot[0], y as i32 - hotspot[1]]),
        Some(shape),
    ))
}
fn bitmap_bits(
    dc: HDC,
    bitmap: HBITMAP,
    width: u32,
    height: u32,
    bits: u16,
    pitch: u32,
) -> Option<Vec<u8>> {
    #[repr(C)]
    struct Info {
        header: BITMAPINFOHEADER,
        colors: [RGBQUAD; 2],
    }
    let mut info = Info {
        header: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: bits,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        colors: [
            RGBQUAD::default(),
            RGBQUAD {
                rgbRed: 255,
                rgbGreen: 255,
                rgbBlue: 255,
                rgbReserved: 0,
            },
        ],
    };
    let mut data = vec![0; pitch as usize * height as usize];
    let rows = unsafe {
        GetDIBits(
            dc,
            bitmap,
            0,
            height,
            Some(data.as_mut_ptr().cast()),
            (&mut info as *mut Info).cast(),
            DIB_RGB_COLORS,
        )
    };
    (rows == height as i32).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{GetGuiResources, GR_GDIOBJECTS};
    fn roundtrip() {
        let dc = Dc {
            handle: unsafe { CreateCompatibleDC(None) },
            screen: false,
        };
        assert!(!dc.handle.0.is_null());
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 2,
                biHeight: -2,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = std::ptr::null_mut();
        let bitmap = Bitmap(
            unsafe {
                CreateDIBSection(Some(dc.handle), &info, DIB_RGB_COLORS, &mut pixels, None, 0)
            }
            .unwrap(),
        );
        let expected = [1u8, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        unsafe {
            std::ptr::copy_nonoverlapping(expected.as_ptr(), pixels.cast::<u8>(), expected.len());
        }
        let result = bitmap_bits(dc.handle, bitmap.0, 2, 2, 32, 8).unwrap();
        assert_eq!(result.len(), expected.len());
        for (actual, expected) in result.chunks_exact(4).zip(expected.chunks_exact(4)) {
            assert_eq!(
                &actual[..3],
                &expected[..3],
                "top-down BGRA rows must retain channel order"
            );
        }
    }
    #[test]
    fn bitmap_readback_keeps_rows_and_releases_owned_gdi_objects() {
        // Only private memory bitmaps: no display DC, input, visible window or
        // desktop switch. Repeated reads must not accumulate GDI handles.
        roundtrip();
        let process = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
        let before = unsafe { GetGuiResources(process, GR_GDIOBJECTS) };
        for _ in 0..30 {
            roundtrip();
        }
        assert_eq!(unsafe { GetGuiResources(process, GR_GDIOBJECTS) }, before);
    }
}
