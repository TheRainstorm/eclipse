//! Read-only cursor shape from Message15/SystemStateChange field2.
//! Official Windows descriptors and UU Linux cursor.rs agree on this layout.
use crate::control_wire::{decode_wire, WireValue};
pub use uu_core::cursor::Shape;
pub fn parse(data: &[u8]) -> Option<Shape> {
    fn message<'a>(fields: &[(u32, WireValue<'a>)], id: u32) -> Option<&'a [u8]> {
        fields.iter().find_map(|(n, v)| {
            if *n == id {
                if let WireValue::Bytes(b) = v {
                    Some(*b)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }
    let outer = decode_wire(data).ok()?;
    let change = decode_wire(message(&outer, 15)?).ok()?;
    let fields = decode_wire(message(&change, 2)?).ok()?;
    let number = |id| {
        fields
            .iter()
            .find_map(|(n, v)| {
                if *n == id {
                    if let WireValue::Varint(v) = v {
                        Some(*v)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap_or(0)
    };
    let scale = |id| {
        fields
            .iter()
            .find_map(|(n, v)| {
                if *n == id {
                    if let WireValue::Fixed64(v) = v {
                        Some(f64::from_le_bytes(*v))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .filter(|v| v.is_finite() && *v >= 0.1 && *v <= 16.0)
            .unwrap_or(1.0)
    };
    let width = number(3);
    let height = number(4);
    let image = message(&fields, 5).unwrap_or_default();
    if width > 512 || height > 512 || image.len() > 1024 * 1024 {
        return None;
    }
    Some(Shape {
        hotspot: [number(1) as i32, number(2) as i32],
        size: [width as u32, height as u32],
        image: image.to_vec(),
        kind: number(6) as i32,
        scale: [scale(7), scale(8)],
        screen: number(9) as i32,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_wire::{bytes, envelope, int};
    #[test]
    fn shape_preserves_hotspot_screen_and_rejects_oversized_image() {
        let mut shape = vec![];
        int(1, 4, &mut shape);
        int(2, 2, &mut shape);
        int(3, 32, &mut shape);
        int(4, 24, &mut shape);
        int(9, 42, &mut shape);
        bytes(5, b"png", &mut shape);
        let wrap = |shape: &[u8]| {
            let mut change = vec![];
            bytes(2, shape, &mut change);
            envelope(1, 15, &change)
        };
        let parsed = parse(&wrap(&shape)).unwrap();
        assert_eq!(parsed.hotspot, [4, 2]);
        assert_eq!(parsed.screen, 42);
        assert_eq!(parsed.scale, [1.0, 1.0]);
        let mut huge = vec![];
        int(3, 513, &mut huge);
        assert!(parse(&wrap(&huge)).is_none());
        assert!(parse(&envelope(1, 3, b"x")).is_none());
    }
}
