//! Read-only DMA-BUF download. Avoid VAImage's CPU detile/retile round trip.
//! Only plain linear NV12 and uncompressed Intel Y tiling are accepted; other
//! modifiers keep the driver's VAImage path. This is still a CPU download.
use memmap2::{MmapOptions, MmapRaw};
use std::{collections::HashMap, fs::File, io, os::fd::AsRawFd, os::unix::fs::MetadataExt};

const LINEAR: u64 = 0;
const INTEL_Y: u64 = 0x0100_0000_0000_0002;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const READ: u64 = 1;
const END: u64 = 4;

fn sync(fd: &File, flags: u64) -> Result<(), String> {
    loop {
        // Linux dma-buf.h: _IOW('b', 0, struct dma_buf_sync { __u64 flags; }).
        let status = unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC, &flags) };
        if status == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
            return Err(format!("DMA-BUF read synchronization: {error}"));
        }
    }
}

/// Map only the allocations that belong to this decoder pool. Its original
/// DMA-BUF fds stay alive, including between successive exports of the same
/// allocation. Ambiguous or unfamiliar identities still work with a one-shot map.
#[derive(Default)]
pub(super) struct Mappings {
    slots: HashMap<(u64, u64), Slot>,
}

enum Slot {
    Unique { size: usize, map: Option<MmapRaw> },
    Ambiguous,
}

fn identity(file: &File) -> Option<(u64, u64)> {
    let stat = file.metadata().ok()?;
    Some((stat.dev(), stat.ino()))
}

impl Mappings {
    pub(super) fn register(&mut self, file: &File, size: usize) {
        if let Some(key) = identity(file) {
            self.slots
                .entry(key)
                .and_modify(|slot| *slot = Slot::Ambiguous)
                .or_insert(Slot::Unique { size, map: None });
        }
    }

    fn with_read<T>(
        &mut self,
        file: &File,
        size: usize,
        visible_end: usize,
        read: impl FnOnce(&[u8]) -> Result<T, String>,
    ) -> Result<T, String> {
        if size == 0 || size > 128 * 1024 * 1024 {
            return Err("DMA-BUF allocation is empty or exceeds the download limit".into());
        }
        let map_file = |length| {
            MmapOptions::new()
                .len(length)
                .map_raw_read_only(file)
                .map_err(|error| format!("mapping DMA-BUF: {error}"))
        };
        let transient;
        let map = match identity(file).and_then(|key| self.slots.get_mut(&key)) {
            Some(Slot::Unique {
                size: expected,
                map,
            }) if visible_end <= *expected && *expected <= size => {
                // VA can round the same imported object up on re-export.
                // Map the original allocation only when both exports cover
                // the entire visible image, including tile padding.
                if map.is_none() {
                    *map = Some(map_file(*expected)?);
                }
                map.as_ref().unwrap()
            }
            _ => {
                transient = map_file(size)?;
                &transient
            }
        };
        sync(file, READ)?;
        let access = ReadAccess(Some(file));
        // The decoder synchronized the surface. Only borrow its bytes between
        // START/END; the raw mapping can stay installed while the GPU writes.
        let result = read(unsafe { std::slice::from_raw_parts(map.as_ptr(), map.len()) });
        access.finish()?;
        result
    }
}

struct ReadAccess<'a>(Option<&'a File>);
impl ReadAccess<'_> {
    fn finish(mut self) -> Result<(), String> {
        sync(self.0.take().unwrap(), READ | END)
    }
}
impl Drop for ReadAccess<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.0 {
            let _ = sync(file, READ | END);
        }
    }
}

/// The caller retains and synchronizes the decoded handle before exporting.
pub(super) fn download(
    mut descriptor: cros_libva::DrmPrimeSurfaceDescriptor,
    mappings: &mut Mappings,
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
    let object = descriptor.objects.pop().unwrap();
    if !matches!(object.drm_format_modifier, LINEAR | INTEL_Y) {
        return Err("DMA-BUF modifier requires the driver download path".into());
    }
    let layer = &descriptor.layers[0];
    read_semiplanar(
        &File::from(object.fd),
        object.size as usize,
        &layer.offset[..2],
        &layer.pitch[..2],
        object.drm_format_modifier,
        [width, height],
        1,
        mappings,
        pool,
    )
}

/// The caller retains the already-synchronized VA lease while an EGL import is being rejected.
pub(crate) fn read_export(
    frame: &crate::dma_buf::Frame,
    size: [u32; 2],
) -> Result<bytes::Bytes, String> {
    if !frame.valid(size) {
        return Err("invalid exported two-plane surface".into());
    }
    let [x, y] = frame.origin;
    let [width, height] = size;
    let bytes = frame.format.row_bytes(1);
    let bounding = [x + width, y + height]; // valid() already checked all extents.
    let pixels = read_semiplanar(
        &frame.file,
        frame.allocation_size as usize,
        &frame.offset,
        &frame.pitch,
        frame.modifier,
        bounding,
        bytes,
        &mut Mappings::default(),
        &Default::default(),
    )?;
    if frame.origin == [0, 0] {
        return Ok(pixels);
    }
    // Only an import failure takes this path. Crop the tightly packed bounding
    // rectangle after detiling, respecting the original chroma plane boundary.
    let stride = (bounding[0] * bytes) as usize;
    let (luma, chroma) = pixels.split_at(stride * bounding[1] as usize);
    super::pack_420(
        &[luma, chroma],
        &[stride, stride],
        width,
        height,
        frame.format,
        frame.origin,
    )
    .map(bytes::Bytes::from)
}

fn read_semiplanar(
    file: &File,
    allocation_size: usize,
    offset: &[u32],
    pitch: &[u32],
    modifier: u64,
    [width, height]: [u32; 2],
    bytes_per_component: u32,
    mappings: &mut Mappings,
    pool: &crate::hw::pixels::Pool,
) -> Result<bytes::Bytes, String> {
    let length = sample_count(width as usize, height as usize)? * bytes_per_component as usize;
    // Validate both planes before any mapping or destination modification.
    let (w, h) = ((width * bytes_per_component) as usize, height as usize);
    let mut visible_end = 0;
    for (p, rows) in [h, h / 2].into_iter().enumerate() {
        visible_end = visible_end.max(validate_plane(
            allocation_size,
            offset[p] as usize,
            pitch[p] as usize,
            w,
            rows,
            modifier,
        )?);
    }
    let mut pixels = pool.take(length);
    mappings.with_read(file, allocation_size, visible_end, |source| {
        let (luma, chroma) = pixels.as_mut().split_at_mut(w * h);
        for (p, destination) in [luma, chroma].into_iter().enumerate() {
            copy_plane(
                source,
                offset[p] as usize,
                pitch[p] as usize,
                w,
                destination.len() / w,
                modifier,
                destination,
            )?;
        }
        Ok(())
    })?;
    Ok(bytes::Bytes::from_owner(pixels))
}

fn sample_count(w: usize, h: usize) -> Result<usize, String> {
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || w > 8192 || h > 8192 {
        return Err("4:2:0 download dimensions must be even and within 8192".into());
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
) -> Result<usize, String> {
    if w == 0 || h == 0 || pitch < w || pitch > 1024 * 1024 {
        return Err("invalid DMA-BUF plane dimensions or pitch".into());
    }
    let end = position(pitch, w - 1, h - 1, modifier)
        .and_then(|last| offset.checked_add(last)?.checked_add(1));
    end.filter(|end| *end <= size)
        .ok_or_else(|| "DMA-BUF plane exceeds its exported object".into())
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
    if modifier == LINEAR {
        for (y, row) in destination.chunks_exact_mut(w).enumerate() {
            let start = offset + y * pitch;
            row.copy_from_slice(&source[start..start + w]);
        }
        return Ok(());
    }

    // Read all 32 row segments of an Intel Y column before moving to the next
    // column. The last group can have fewer visible rows than a complete tile.
    const TILE_ROWS: usize = 32;
    let whole = w / 16 * 16;
    for (band, rows) in destination.chunks_mut(w * TILE_ROWS).enumerate() {
        let base = offset + band * pitch * TILE_ROWS;
        for x in (0..whole).step_by(16) {
            let column = base + x / 16 * 512;
            for (dy, row) in rows.chunks_exact_mut(w).enumerate() {
                let start = column + dy * 16;
                row[x..x + 16].copy_from_slice(&source[start..start + 16]);
            }
        }
        if whole < w {
            let column = base + whole / 16 * 512;
            for (dy, row) in rows.chunks_exact_mut(w).enumerate() {
                let start = column + dy * 16;
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
    fn mappings_require_a_unique_pool_object_that_covers_the_visible_image() {
        let file = File::open(std::env::current_exe().unwrap()).unwrap();
        let key = identity(&file).unwrap();
        let mut mappings = Mappings::default();
        mappings.register(&file, 4096);

        // A regular file can be mapped but cannot start DMA-BUF CPU access.
        // No read callback may observe its bytes on either path.
        let unreachable_read = |_: &[u8]| -> Result<(), String> { panic!("unsynchronized read") };
        assert!(mappings
            .with_read(&file, 2048, 2048, unreachable_read)
            .is_err());
        assert!(mappings
            .with_read(&file, 8192, 4100, unreachable_read)
            .is_err());
        assert!(matches!(
            mappings.slots[&key],
            Slot::Unique { map: None, .. }
        ));
        assert!(mappings
            .with_read(&file, 8192, 4000, unreachable_read)
            .is_err());
        assert!(matches!(
            mappings.slots[&key],
            Slot::Unique { map: Some(_), .. }
        ));

        mappings.register(&file.try_clone().unwrap(), 4096);
        assert!(matches!(mappings.slots[&key], Slot::Ambiguous));
        assert!(mappings
            .with_read(&file, 8192, 4000, unreachable_read)
            .is_err());
        assert_eq!(mappings.slots.len(), 1);
    }

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
    fn intel_y_bands_match_visible_pixels_across_padded_tile_rows() {
        let (offset, pitch, width) = (63, 384, 146);
        for height in [1, 7, 8, 9, 17, 31, 32, 33, 64, 65] {
            // Supply exactly the visible extent. A partial column or band
            // must not read a complete 16-byte segment past the allocation.
            let last = position(pitch, width - 1, height - 1, INTEL_Y).unwrap();
            let source: Vec<u8> = (0..=offset + last)
                .map(|index| (index ^ (index >> 7) ^ (index >> 14)) as u8)
                .collect();
            let expected: Vec<u8> = (0..height)
                .flat_map(|y| (0..width).map(move |x| position(pitch, x, y, INTEL_Y).unwrap()))
                .map(|index| source[offset + index])
                .collect();
            let mut destination = vec![0; width * height];
            copy_plane(
                &source,
                offset,
                pitch,
                width,
                height,
                INTEL_Y,
                &mut destination,
            )
            .unwrap();
            assert_eq!(destination, expected, "height {height}");
        }
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
        assert!(sample_count(0, 2).is_err());
        assert!(sample_count(8194, 2).is_err());
    }
}
