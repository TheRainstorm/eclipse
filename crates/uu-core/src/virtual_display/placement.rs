//! Positions belong to logical display definitions, not Windows route numbers.
use super::{Error, Position, Result, Spec};

fn overlaps(p: Position, s: Spec, other: &(Position, u32, u32)) -> bool {
    let (q, w, h) = *other;
    i64::from(p.x) < i64::from(q.x) + i64::from(w)
        && i64::from(q.x) < i64::from(p.x) + i64::from(s.width)
        && i64::from(p.y) < i64::from(q.y) + i64::from(h)
        && i64::from(q.y) < i64::from(p.y) + i64::from(s.height)
}
fn adjacent(p: Position, s: Spec, other: &(Position, u32, u32)) -> bool {
    let (q, w, h) = *other;
    let horizontal = i64::from(p.x) < i64::from(q.x) + i64::from(w)
        && i64::from(q.x) < i64::from(p.x) + i64::from(s.width);
    let vertical = i64::from(p.y) < i64::from(q.y) + i64::from(h)
        && i64::from(q.y) < i64::from(p.y) + i64::from(s.height);
    (horizontal
        && (i64::from(p.y) + i64::from(s.height) == i64::from(q.y)
            || i64::from(q.y) + i64::from(h) == i64::from(p.y)))
        || (vertical
            && (i64::from(p.x) + i64::from(s.width) == i64::from(q.x)
                || i64::from(q.x) + i64::from(w) == i64::from(p.x)))
}
pub(super) fn choose(
    spec: Spec,
    remembered: Option<Position>,
    rectangles: &[(Position, u32, u32)],
) -> Result<Position> {
    if let Some(position) = remembered {
        if rectangles.iter().all(|r| !overlaps(position, spec, r))
            && (rectangles.is_empty() || rectangles.iter().any(|r| adjacent(position, spec, r)))
        {
            return Ok(position);
        }
    }
    let primary = rectangles.iter().find(|(p, _, _)| p.x == 0 && p.y == 0);
    let mut x = primary.map_or(0, |(_, w, _)| i64::from(*w));
    loop {
        let position = Position {
            x: i32::try_from(x)
                .map_err(|_| Error::Platform("desktop coordinate overflow".into()))?,
            y: 0,
        };
        let collisions: Vec<_> = rectangles
            .iter()
            .filter(|r| overlaps(position, spec, r))
            .collect();
        if collisions.is_empty() {
            return Ok(position);
        }
        x = collisions
            .into_iter()
            .map(|(p, w, _)| i64::from(p.x) + i64::from(*w))
            .max()
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_removed_bridge_does_not_restore_an_isolated_rectangle() {
        let screens = [(Position { x: 0, y: 0 }, 1920, 1080)];
        assert_eq!(
            choose(Spec::default(), Some(Position { x: 3840, y: 0 }), &screens).unwrap(),
            Position { x: 1920, y: 0 }
        );
    }
    #[test]
    fn keeps_above_primary_and_adds_new_to_primary_right() {
        let spec = Spec::default();
        let primary = (Position { x: 0, y: 0 }, 3072, 1920);
        let above = (Position { x: 0, y: -1080 }, 1920, 1080);
        assert_eq!(choose(spec, Some(above.0), &[primary]).unwrap(), above.0);
        assert_eq!(
            choose(spec, None, &[primary, above]).unwrap(),
            Position { x: 3072, y: 0 }
        );
        assert_eq!(
            choose(spec, Some(Position { x: 0, y: 0 }), &[primary, above]).unwrap(),
            Position { x: 3072, y: 0 }
        );
    }
    #[test]
    fn restore_off_skips_occupied_right_hand_space_without_touching_other_screens() {
        let screens = [
            (Position { x: 0, y: 0 }, 1920, 1080),
            (Position { x: 1920, y: 0 }, 2560, 1440),
            (Position { x: 9000, y: -2160 }, 3840, 2160),
        ];
        assert_eq!(
            choose(Spec::default(), None, &screens).unwrap(),
            Position { x: 4480, y: 0 }
        );
        assert_eq!(
            choose(Spec::default(), Some(Position { x: -1920, y: 0 }), &screens).unwrap(),
            Position { x: -1920, y: 0 }
        );
    }
}
