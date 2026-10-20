/// Copy the visible top-left NV12 image out of an aligned GPU allocation.
/// UV starts after the allocation's full luma height, even when the visible
/// image is shorter (for example 1080 lines in a 1088-line decode texture).
#[cfg(test)]
pub(super) fn crop_nv12(
    source: &[u8],
    pitch: u32,
    allocated_height: u32,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .map(|n| n / 2)
        .ok_or("NV12 size overflow")?;
    if size > source.len() {
        return Err("truncated NV12 surface".into());
    }
    let mut pixels = vec![0; size];
    crop_nv12_into(source, pitch, allocated_height, width, height, &mut pixels)?;
    Ok(pixels)
}

/// Write every visible byte into initialized, caller-owned reusable storage.
pub(super) fn crop_nv12_into(
    source: &[u8],
    pitch: u32,
    allocated_height: u32,
    width: u32,
    height: u32,
    pixels: &mut [u8],
) -> Result<(), String> {
    if width == 0
        || height == 0
        || width % 2 != 0
        || height % 2 != 0
        || width > pitch
        || height > allocated_height
        || allocated_height % 2 != 0
    {
        return Err(format!("invalid NV12 layout: {width}x{height}, pitch {pitch}, allocation height {allocated_height}"));
    }
    let (pitch, allocated_height, w, h) = (
        pitch as usize,
        allocated_height as usize,
        width as usize,
        height as usize,
    );
    let uv_start = pitch
        .checked_mul(allocated_height)
        .ok_or("NV12 layout overflow")?;
    let required = pitch
        .checked_mul(h / 2)
        .and_then(|n| uv_start.checked_add(n))
        .ok_or("NV12 layout overflow")?;
    if source.len() < required {
        return Err("truncated NV12 surface".into());
    }
    if pixels.len() != w * h * 3 / 2 {
        return Err("incorrect NV12 destination size".into());
    }
    for row in 0..h {
        pixels[row * w..(row + 1) * w].copy_from_slice(&source[row * pitch..row * pitch + w]);
    }
    for row in 0..h / 2 {
        let destination = w * h + row * w;
        let source_row = uv_start + row * pitch;
        pixels[destination..destination + w].copy_from_slice(&source[source_row..source_row + w]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reusable_output_is_fully_overwritten_and_bad_size_is_unchanged() {
        let source = [
            1, 2, 99, 99, 3, 4, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 10, 20, 99, 99, 99, 99, 99,
            99,
        ];
        let mut pixels = [77; 6];
        crop_nv12_into(&source, 4, 4, 2, 2, &mut pixels).unwrap();
        assert_eq!(pixels, [1, 2, 3, 4, 10, 20]);
        let mut short = [77; 5];
        assert!(crop_nv12_into(&source, 4, 4, 2, 2, &mut short).is_err());
        assert_eq!(short, [77; 5]);
    }
    #[test]
    fn cropped_nv12_skips_padded_rows_before_chroma() {
        // Visible 2x2 image in a pitch-4, height-4 allocation. Padding must
        // neither reach the displayed luma nor be mistaken for the UV plane.
        let source = [
            1, 2, 99, 99, 3, 4, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 10, 20, 99, 99, 99, 99, 99,
            99,
        ];
        assert_eq!(
            crop_nv12(&source, 4, 4, 2, 2).unwrap(),
            [1, 2, 3, 4, 10, 20]
        );
    }

    #[test]
    fn invalid_and_truncated_nv12_surfaces_are_rejected() {
        assert!(crop_nv12(&[0; 24], 4, 4, 6, 2).is_err());
        assert!(crop_nv12(&[0; 24], 4, 4, 2, 6).is_err());
        assert!(crop_nv12(&[0; 24], 4, 4, 1, 2).is_err());
        assert!(crop_nv12(&[0; 19], 4, 4, 2, 2).is_err());
    }
}
