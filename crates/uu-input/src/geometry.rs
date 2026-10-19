use crate::InputError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopRect {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}
impl DesktopRect {
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Result<Self, InputError> {
        if width == 0
            || height == 0
            || i64::from(left) + i64::from(width) - 1 > i64::from(i32::MAX)
            || i64::from(top) + i64::from(height) - 1 > i64::from(i32::MAX)
        {
            return Err(InputError::Backend("invalid desktop rectangle".into()));
        }
        Ok(Self {
            left,
            top,
            width,
            height,
        })
    }
    /// Map inclusive normalized coordinates into the selected capture region.
    pub fn point(self, x: u16, y: u16) -> (i32, i32) {
        let axis = |origin: i32, size: u32, value: u16| {
            (i64::from(origin) + ((u64::from(value) * u64::from(size - 1) + 32767) / 65535) as i64)
                as i32
        };
        (
            axis(self.left, self.width, x),
            axis(self.top, self.height, y),
        )
    }
    #[cfg(any(windows, test))]
    pub(crate) fn windows_absolute(self, x: i32, y: i32) -> (i32, i32) {
        let axis = |origin: i32, size: u32, value: i32| {
            let pixel = (i64::from(value) - i64::from(origin)).clamp(0, i64::from(size) - 1) as u64;
            ((pixel * 65536 + 32768) / u64::from(size)).min(65535) as i32
        };
        (
            axis(self.left, self.width, x),
            axis(self.top, self.height, y),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_source_and_windows_virtual_desktop_preserve_negative_origin_and_edges() {
        let virtual_screen = DesktopRect::new(-1920, -1080, 3840, 2160).unwrap();
        let source = DesktopRect::new(-1920, -1080, 1920, 1080).unwrap();
        assert_eq!(source.point(0, 0), (-1920, -1080));
        assert_eq!(source.point(65535, 65535), (-1, -1));
        for (x, y) in [(-1920, -1080), (-1, -1), (0, 0), (1919, 1079)] {
            let (nx, ny) = virtual_screen.windows_absolute(x, y);
            assert_eq!(
                (
                    nx as i64 * 3840 / 65536 - 1920,
                    ny as i64 * 2160 / 65536 - 1080
                ),
                (i64::from(x), i64::from(y))
            );
        }
        assert!(DesktopRect::new(0, 0, 0, 1080).is_err());
        assert!(DesktopRect::new(i32::MAX, 0, 2, 2).is_err());
    }
}
