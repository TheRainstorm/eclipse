//! Read-only DMA-BUF download. Avoid VAImage's CPU detile/retile round trip.
//! Only plain linear NV12 and uncompressed Intel Y tiling are accepted; other
//! modifiers keep the driver's VAImage path. This is still a CPU download.
use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    ptr::NonNull,
};

const LINEAR: u64 = 0;
const INTEL_Y: u64 = 0x0100_0000_0000_0002;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const READ: u64 = 1;
const END: u64 = 4;

fn sync(fd: &OwnedFd, flags: u64) -> Result<(), String> {
    loop {
        // Linux dma-buf.h: _IOW('b', 0, struct dma_buf_sync { __u64 flags; }).
        let status = unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &flags) };
        if status == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(format!("DMA-BUF read synchronization: {error}"));
        }
    }
}

struct Mapping<'a> {
    fd: &'a OwnedFd,
    address: NonNull<libc::c_void>,
    size: usize,
}
impl<'a> Mapping<'a> {
    fn new(fd: &'a OwnedFd, size: usize) -> Result<Self, String> {
        if size == 0 || size > 128 * 1024 * 1024 {
            return Err("DMA-BUF allocation is empty or exceeds the download limit".into());
        }
        // The VA surface and its exported descriptor outlive this mapping.
        // Never write to decoded/reference pictures through a CPU mapping.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(format!("mapping DMA-BUF: {}", io::Error::last_os_error()));
        }
        let Some(address) = NonNull::new(raw) else {
            unsafe {
                libc::munmap(raw, size);
            }
            return Err("DMA-BUF mapped at a null address".into());
        };
        if let Err(error) = sync(fd, READ) {
            unsafe {
                libc::munmap(raw, size);
            }
            return Err(error);
        }
        Ok(Self { fd, address, size })
    }
    fn bytes(&self) -> &[u8] {
        // mmap succeeded; the exported object bounds the slice. START/READ
        // has completed and the caller has synchronized the decoded surface.
        unsafe { std::slice::from_raw_parts(self.address.as_ptr().cast(), self.size) }
    }
    fn finish(self) -> Result<(), String> {
        let result = sync(self.fd, READ | END);
        // Drop must always unmap, including an END failure; avoid a second END.
        unsafe {
            libc::munmap(self.address.as_ptr(), self.size);
        }
        std::mem::forget(self);
        result
    }
}
impl Drop for Mapping<'_> {
    fn drop(&mut self) {
        let _ = sync(self.fd, READ | END);
        unsafe {
            libc::munmap(self.address.as_ptr(), self.size);
        }
    }
}

/// The descriptor's FDs are owned by its RAII wrapper and closed by libva.
/// The caller retains and synchronizes the decoded handle before exporting.
pub(super) fn download(
    descriptor: &cros_libva::DrmPrimeSurfaceDescriptor,
    width: u32,
    height: u32,
    pool: &crate::hw::pixels::Pool,
) -> Result<bytes::Bytes, String> {
    if descriptor.fourcc != cros_libva::VA_FOURCC_NV12
        || descriptor.objects.len() != 1
        || descriptor.layers.len() != 1
        || descriptor.layers[0].num_planes != 2
        || descriptor.layers[0].object_index[..2] != [0, 0]
        || width > descriptor.width
        || height > descriptor.height
    {
        return Err("unsupported DMA-BUF NV12 layout".into());
    }
    let object = &descriptor.objects[0];
    if !matches!(object.drm_format_modifier, LINEAR | INTEL_Y) {
        return Err("DMA-BUF modifier requires the driver download path".into());
    }
    let length = nv12_size(width as usize, height as usize)?;
    let layer = &descriptor.layers[0];
    // Validate both planes before any mapping or destination modification.
    let (w, h) = (width as usize, height as usize);
    for (p, rows) in [h, h / 2].into_iter().enumerate() {
        validate_plane(
            object.size as usize,
            layer.offset[p] as usize,
            layer.pitch[p] as usize,
            w,
            rows,
            object.drm_format_modifier,
        )?;
    }
    let mapping = Mapping::new(&object.fd, object.size as usize)?;
    let mut pixels = pool.take(length);
    let (luma, chroma) = pixels.as_mut().split_at_mut(w * h);
    for (p, destination) in [luma, chroma].into_iter().enumerate() {
        copy_plane(
            mapping.bytes(),
            layer.offset[p] as usize,
            layer.pitch[p] as usize,
            w,
            destination.len() / w,
            object.drm_format_modifier,
            destination,
        )?;
    }
    mapping.finish()?;
    Ok(bytes::Bytes::from_owner(pixels))
}

fn nv12_size(w: usize, h: usize) -> Result<usize, String> {
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || w > 8192 || h > 8192 {
        return Err("NV12 download dimensions must be even and within 8192".into());
    }
    Ok(w * h * 3 / 2)
}
fn position(pitch: usize, x: usize, y: usize, modifier: u64) -> Option<usize> {
    if modifier == LINEAR {
        y.checked_mul(pitch)?.checked_add(x)
    } else if modifier == INTEL_Y && pitch % 128 == 0 {
        // Intel Y: 128x32-byte tiles, each holding eight 16-byte columns.
        (y / 32)
            .checked_mul(pitch)?
            .checked_mul(32)?
            .checked_add((x / 128).checked_mul(4096)?)?
            .checked_add(((x % 128) / 16) * 512 + (y % 32) * 16 + x % 16)
    } else {
        None
    }
}
fn validate_plane(
    size: usize,
    offset: usize,
    pitch: usize,
    w: usize,
    h: usize,
    modifier: u64,
) -> Result<(), String> {
    if w == 0 || h == 0 || pitch < w || pitch > 1024 * 1024 {
        return Err("invalid DMA-BUF plane dimensions or pitch".into());
    }
    let end = position(pitch, w - 1, h - 1, modifier)
        .and_then(|last| offset.checked_add(last)?.checked_add(1));
    if end.map_or(true, |end| end > size) {
        return Err("DMA-BUF plane exceeds its exported object".into());
    }
    Ok(())
}
fn copy_plane(
    source: &[u8],
    offset: usize,
    pitch: usize,
    w: usize,
    h: usize,
    modifier: u64,
    destination: &mut [u8],
) -> Result<(), String> {
    validate_plane(source.len(), offset, pitch, w, h, modifier)?;
    if w.checked_mul(h) != Some(destination.len()) {
        return Err("invalid DMA-BUF destination size".into());
    }
    for (y, row) in destination.chunks_exact_mut(w).enumerate() {
        if modifier == LINEAR {
            let start = offset + y * pitch;
            row.copy_from_slice(&source[start..start + w]);
        } else {
            let base = offset + (y / 32) * pitch * 32 + (y % 32) * 16;
            let whole = w / 16 * 16;
            for (column, target) in row[..whole].chunks_exact_mut(16).enumerate() {
                let start = base + (column / 8) * 4096 + (column % 8) * 512;
                target.copy_from_slice(&source[start..start + 16]);
            }
            if whole != w {
                let start = base + (whole / 128) * 4096 + ((whole % 128) / 16) * 512;
                row[whole..].copy_from_slice(&source[start..start + w - whole]);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intel_y_known_tile_columns_rows_and_edges() {
        // Independent fixed offsets at tile, subtile and row boundaries.
        for (x, y, expected) in [
            (0, 0, 0),
            (15, 0, 15),
            (16, 0, 512),
            (127, 0, 3599),
            (0, 1, 16),
            (0, 31, 496),
            (128, 0, 4096),
            (0, 32, 8192),
            (129, 33, 12305),
        ] {
            assert_eq!(position(256, x, y, INTEL_Y), Some(expected));
        }
    }
    #[test]
    fn partial_tiles_padding_and_plane_offsets_do_not_leak_into_pixels() {
        let mut source = vec![99; 64 + 8192];
        // Visible 18x2 bytes; no assumption that visible rows fill tiles.
        source[64..80].fill(1);
        source[64 + 512..64 + 514].fill(2);
        source[64 + 16..64 + 32].fill(3);
        source[64 + 528..64 + 530].fill(4);
        let mut destination = vec![77; 36];
        copy_plane(&source, 64, 256, 18, 2, INTEL_Y, &mut destination).unwrap();
        assert_eq!(
            destination,
            [vec![1; 16], vec![2; 2], vec![3; 16], vec![4; 2]].concat()
        );
    }
    #[test]
    fn linear_planes_respect_offsets_and_row_padding() {
        let mut destination = [77; 4];
        copy_plane(
            &[99, 1, 2, 99, 99, 3, 4],
            1,
            4,
            2,
            2,
            LINEAR,
            &mut destination,
        )
        .unwrap();
        assert_eq!(destination, [1, 2, 3, 4]);
    }
    #[test]
    fn unsupported_compressed_truncated_and_overflowing_layouts_are_rejected_before_copy() {
        for (offset, pitch, w, h, modifier) in [
            (0, 128, 18, 2, INTEL_Y),
            (usize::MAX, 128, 2, 2, INTEL_Y),
            (0, 127, 2, 2, INTEL_Y),
            (0, 128, 2, 2, INTEL_Y + 1),
            (0, 1, 2, 2, LINEAR),
        ] {
            let mut destination = vec![77; w * h];
            assert!(copy_plane(&[0; 32], offset, pitch, w, h, modifier, &mut destination).is_err());
            assert!(destination.iter().all(|b| *b == 77));
        }
        assert!(nv12_size(0, 2).is_err());
        assert!(nv12_size(8194, 2).is_err());
    }
}
