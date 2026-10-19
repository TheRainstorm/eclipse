//! Read-only native screen metadata from the official Windows descriptors.
use crate::control_wire::{decode_wire, WireValue};

fn number(fields: &[(u32, WireValue<'_>)], field: u32) -> u64 {
    fields
        .iter()
        .find_map(|(n, v)| match (n, v) {
            (n, WireValue::Varint(v)) if *n == field => Some(*v),
            _ => None,
        })
        .unwrap_or(0)
}
fn message<'a>(fields: &[(u32, WireValue<'a>)], field: u32) -> Option<&'a [u8]> {
    fields.iter().find_map(|(n, v)| match (n, v) {
        (n, WireValue::Bytes(v)) if *n == field => Some(*v),
        _ => None,
    })
}
fn dimension(fields: &[(u32, WireValue<'_>)], physical: u32, logical: u32) -> u32 {
    [number(fields, physical), number(fields, logical)]
        .into_iter()
        .find(|v| *v > 0 && *v <= i32::MAX as u64)
        .unwrap_or(0) as u32
}
#[derive(Clone, Debug, Default)]
pub struct RemoteScreens {
    pub screens: Vec<RemoteScreen>,
    pub current_id: i32,
}
impl RemoteScreens {
    pub fn selected(&self) -> Option<&RemoteScreen> {
        self.screens.iter().find(|s| s.id == self.current_id)
    }
    pub fn for_track(&self, track: u32) -> Option<&RemoteScreen> {
        self.selected()
            .filter(|s| s.video_track_index == track)
            .or_else(|| self.screens.iter().find(|s| s.video_track_index == track))
    }
}
#[derive(Clone, Debug)]
pub struct RemoteScreen {
    pub id: i32,
    pub refresh_hz: u32,
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub dpi_scale: Option<u32>,
    pub video_track_index: u32,
}
impl RemoteScreen {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            0 => "物理屏",
            1 => "虚拟屏",
            2 => "超级屏",
            _ => "未知屏幕",
        }
    }
}
pub fn screen_sources(data: &[u8]) -> Option<RemoteScreens> {
    let outer = decode_wire(data).ok()?;
    let fields = decode_wire(message(&outer, 7)?).ok()?;
    let mut screens = Vec::new();
    for (_, value) in fields.iter().filter(|(n, _)| *n == 1).take(32) {
        let WireValue::Bytes(data) = value else {
            continue;
        };
        let fields = decode_wire(data).ok()?;
        let rect = message(&fields, 4)
            .and_then(|v| decode_wire(v).ok())
            .unwrap_or_default();
        let dpi_scale = message(&fields, 9)
            .and_then(|v| decode_wire(v).ok())
            .map(|f| number(&f, 1) as u32)
            .filter(|v| *v > 0);
        screens.push(RemoteScreen {
            id: number(&fields, 1) as i32,
            refresh_hz: number(&fields, 2) as u32,
            kind: number(&fields, 5) as u32,
            width: dimension(&rect, 5, 3),
            height: dimension(&rect, 6, 4),
            dpi_scale,
            video_track_index: number(&fields, 12) as u32,
        });
    }
    Some(RemoteScreens {
        screens,
        current_id: number(&fields, 2) as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_wire::{bytes, int};
    #[test]
    fn native_screen_ids_follow_advertised_tracks_instead_of_arrival_order() {
        let mut payload = Vec::new();
        for (id, track) in [(42, 2), (7, 0), (11, 1)] {
            let mut screen = Vec::new();
            int(1, id, &mut screen);
            int(12, track, &mut screen);
            bytes(1, &screen, &mut payload);
        }
        int(2, 42, &mut payload);
        let parsed = screen_sources(&crate::control_wire::envelope(1, 7, &payload)).unwrap();
        assert_eq!(parsed.for_track(0).unwrap().id, 7);
        assert_eq!(parsed.for_track(1).unwrap().id, 11);
        assert_eq!(parsed.for_track(2).unwrap().id, 42);
        assert!(parsed.for_track(3).is_none());
    }
    #[test]
    fn screen_metadata_uses_selected_id_not_first_physical_screen() {
        let mut payload = Vec::new();
        for (id, kind, rate) in [(1, 0, 30), (2, 2, 120)] {
            let mut screen = Vec::new();
            int(1, id, &mut screen);
            int(2, rate, &mut screen);
            int(5, kind, &mut screen);
            let mut rect = Vec::new();
            int(3, 3000, &mut rect); // Logical dimensions are not physical dimensions.
            int(5, 1920, &mut rect);
            bytes(4, &rect, &mut screen);
            bytes(1, &screen, &mut payload);
        }
        int(2, 2, &mut payload);
        let parsed = screen_sources(&crate::control_wire::envelope(1, 7, &payload)).unwrap();
        let selected = parsed.selected().unwrap();
        assert_eq!(selected.kind_label(), "超级屏");
        assert_eq!(selected.refresh_hz, 120);
        assert_eq!(selected.width, 1920);
        assert_eq!(parsed.for_track(0).unwrap().id, 2);
        assert!(screen_sources(&crate::control_wire::echo_request(1)).is_none());
    }
}
