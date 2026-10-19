//! Checked CPU transfer for desktop surfaces, including padding and orientation.
use crate::CaptureError;
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    None,
    Clockwise90,
    Half,
    Clockwise270,
}

pub(crate) fn bgra_rows(
    data: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
    rotation: Rotation,
) -> Result<(usize, usize, Vec<u8>), CaptureError> {
    let error = || CaptureError::Backend("invalid or incomplete desktop pixel buffer".into());
    let row = width.checked_mul(4).ok_or_else(error)?;
    let needed = height
        .checked_sub(1)
        .and_then(|h| h.checked_mul(pitch))
        .and_then(|n| n.checked_add(row))
        .ok_or_else(error)?;
    let total = row
        .checked_mul(height)
        .filter(|n| *n <= 256 * 1024 * 1024)
        .ok_or_else(error)?;
    if width == 0 || pitch < row || data.len() < needed {
        return Err(error());
    }
    let (ow, oh) = if matches!(rotation, Rotation::Clockwise90 | Rotation::Clockwise270) {
        (height, width)
    } else {
        (width, height)
    };
    let mut result = vec![0u8; total];
    if rotation == Rotation::None {
        for y in 0..height {
            result[y * row..(y + 1) * row].copy_from_slice(&data[y * pitch..y * pitch + row]);
        }
        for pixel in result.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    } else {
        for y in 0..height {
            for x in 0..width {
                let (dx, dy) = match rotation {
                    Rotation::None => (x, y),
                    Rotation::Clockwise90 => (height - 1 - y, x),
                    Rotation::Half => (width - 1 - x, height - 1 - y),
                    Rotation::Clockwise270 => (y, width - 1 - x),
                };
                let input = y * pitch + x * 4;
                let output = (dy * ow + dx) * 4;
                result[output..output + 3].copy_from_slice(&data[input..input + 3]);
                result[output + 3] = 255;
            }
        }
    }
    Ok((ow, oh, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_rotations_preserve_pixel_identity_and_handle_row_padding() {
        let mut data = Vec::new();
        for row in [[1u8, 2], [3, 4], [5, 6]] {
            for value in row {
                data.extend([value, 0, 0, 0]);
            }
            data.extend([99, 99, 99, 99]);
        }
        for (rotation, shape, expected) in [
            (Rotation::None, (2, 3), vec![1, 2, 3, 4, 5, 6]),
            (Rotation::Clockwise90, (3, 2), vec![5, 3, 1, 6, 4, 2]),
            (Rotation::Half, (2, 3), vec![6, 5, 4, 3, 2, 1]),
            (Rotation::Clockwise270, (3, 2), vec![2, 4, 6, 1, 3, 5]),
        ] {
            let (w, h, pixels) = bgra_rows(&data, 2, 3, 12, rotation).unwrap();
            assert_eq!((w, h), shape);
            assert_eq!(
                pixels.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(),
                expected
            );
            assert!(pixels.chunks_exact(4).all(|p| p[3] == 255));
        }
    }
    #[test]
    fn truncated_rows_short_pitch_and_overflow_are_rejected() {
        assert!(bgra_rows(&[0; 16], 2, 2, 4, Rotation::None).is_err());
        assert!(bgra_rows(&[0; 15], 2, 2, 8, Rotation::None).is_err());
        assert!(bgra_rows(&[], usize::MAX, 2, 8, Rotation::None).is_err());
        assert!(bgra_rows(&[], 0, 0, 0, Rotation::None).is_err());
    }
}
