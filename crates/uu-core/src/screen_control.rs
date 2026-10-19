//! Screen metadata and super-screen RPCs from the official Windows descriptors.
use crate::control_wire::{bytes, decode_wire, int, CaptureFrameRate, WireValue};

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
    [number(fields, physical), number(fields, logical)].into_iter()
        .find(|v| *v > 0 && *v <= i32::MAX as u64).unwrap_or(0) as u32
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
}
#[derive(Clone, Debug)]
pub struct RemoteScreen {
    pub id: i32,
    pub refresh_hz: u32,
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub dpi_scale: Option<u32>,
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
        });
    }
    Some(RemoteScreens {
        screens,
        current_id: number(&fields, 2) as i32,
    })
}

pub fn super_screen_request(
    id: u64,
    enter: bool,
    size: (u32, u32),
    fps: CaptureFrameRate,
    dpi: Option<u32>,
) -> Vec<u8> {
    let mut body = Vec::new();
    if enter {
        int(1, 1, &mut body); // EnterReason.REASON_MANUAL
        let mut resolution = Vec::new();
        int(1, size.0 as u64, &mut resolution);
        int(2, size.1 as u64, &mut resolution);
        bytes(2, &resolution, &mut body);
        if let Some(dpi) = dpi {
            int(3, dpi as u64, &mut body);
        }
        let mut rate = Vec::new();
        int(1, fps.fps() as u64, &mut rate);
        bytes(4, &rate, &mut body);
    }
    let mut header = Vec::new();
    int(1, id, &mut header);
    let mut rpc = Vec::new();
    bytes(1, &header, &mut rpc);
    bytes(if enter { 26 } else { 14 }, &body, &mut rpc);
    let mut out = Vec::new();
    int(1, id, &mut out);
    int(2, crate::control_wire::now_ms(), &mut out);
    bytes(21, &rpc, &mut out);
    out
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenModeResponse {
    pub id: u64,
    pub enter: bool,
    pub error_code: i32,
}
pub fn super_screen_response(data: &[u8]) -> Option<ScreenModeResponse> {
    let outer = decode_wire(data).ok()?;
    let rpc = decode_wire(message(&outer, 22)?).ok()?;
    let header = decode_wire(message(&rpc, 1)?).ok()?;
    let (enter, body) = if let Some(body) = message(&rpc, 22) {
        (true, body)
    } else {
        (false, message(&rpc, 10)?)
    };
    Some(ScreenModeResponse {
        id: number(&header, 1),
        enter,
        error_code: number(&decode_wire(body).ok()?, 1) as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(screen_sources(&crate::control_wire::echo_request(1)).is_none());
    }
    #[test]
    fn screen_rpc_has_official_oneof_count_and_matching_errors() {
        for enter in [true, false] {
            let request = super_screen_request(
                5001,
                enter,
                (1920, 1080),
                CaptureFrameRate::new(120).unwrap(),
                Some(100),
            );
            let fields = decode_wire(&request).unwrap();
            let rpc = decode_wire(message(&fields, 21).unwrap()).unwrap();
            let body = decode_wire(message(&rpc, if enter { 26 } else { 14 }).unwrap()).unwrap();
            if enter {
                let fps = decode_wire(message(&body, 4).unwrap()).unwrap();
                assert_eq!(number(&fps, 1), 120);
                assert_eq!(number(&body, 1), 1);
            } else {
                assert!(body.is_empty());
            }
            for error in [0, 17] {
                let mut head = Vec::new();
                int(1, 5001, &mut head);
                let mut result = Vec::new();
                if error != 0 {
                    int(1, error, &mut result);
                }
                let mut rpc = Vec::new();
                bytes(1, &head, &mut rpc);
                bytes(if enter { 22 } else { 10 }, &result, &mut rpc);
                assert_eq!(
                    super_screen_response(&crate::control_wire::envelope(8, 22, &rpc)),
                    Some(ScreenModeResponse {
                        id: 5001,
                        enter,
                        error_code: error as i32
                    })
                );
            }
        }
    }
}
