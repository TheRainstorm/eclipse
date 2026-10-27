//! Remote screen inventory and explicit display RPCs from Windows descriptors.
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
pub use uu_core::screen_control::{RemoteScreens,RemoteScreen,DisplayMode};
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
        let id=number(&fields,1) as i32;
        if id<0 {continue;}
        if screens.iter().any(|s:&RemoteScreen|s.id==id){return None;}
        screens.push(RemoteScreen {
            id,
            display_name: message(&fields,10).and_then(|v|std::str::from_utf8(v).ok()).unwrap_or("").chars().filter(|c|!c.is_control()).take(96).collect(),
            primary: number(&fields,7)!=0,
            refresh_hz: number(&fields, 2) as u32,
            kind: number(&fields, 5) as u32,
            width: dimension(&rect, 5, 3),
            height: dimension(&rect, 6, 4),
            dpi_scale,
            video_track_index: number(&fields, 12) as i32,
        });
    }
    Some(RemoteScreens {
        screens,
        current_id: number(&fields, 2) as i32,
    })
}

/// Legacy desktop switches notify Message.8 CaptureChange, not ScreenSources.
/// Desktop capture uses track 0; type_value is the actual source ID.
pub fn desktop_capture_change(data: &[u8]) -> Option<i32> {
    let outer = decode_wire(data).ok()?;
    let change = decode_wire(message(&outer, 8)?).ok()?;
    let id = number(&change, 2) as i32;
    (number(&change, 1) == 0 && id >= 0).then_some(id)
}

fn request(seq:u64,case:u32,payload:&[u8])->Vec<u8> {
    use crate::control_wire::{bytes,int};
    let mut header=Vec::new();int(1,seq,&mut header);
    let mut rpc=Vec::new();bytes(1,&header,&mut rpc);bytes(case,payload,&mut rpc);
    let mut out=Vec::new();int(1,seq,&mut out);int(2,crate::control_wire::now_ms(),&mut out);bytes(21,&rpc,&mut out);out
}
/// Source-only CaptureConfig flag8 binds an existing monitor to capture.
/// It does not set FPS/quality/resolution or change Windows display topology.
pub fn activate_screen(seq:u64,screen_id:i32)->Vec<u8> {
    let mut body=Vec::new();crate::control_wire::int(1,3,&mut body);
    crate::control_wire::int(5,screen_id as i64 as u64,&mut body);
    crate::control_wire::int(8,8,&mut body);
    crate::control_wire::envelope(seq,9,&body)
}
/// Ask the host to send an existing track; does not create/disable monitors.
pub fn send_video_track(seq:u64,index:u32)->Vec<u8> {
    let mut body=Vec::new();crate::control_wire::int(1,index.into(),&mut body);request(seq,15,&body)
}
pub fn enter_exclusive(seq:u64,width:u32,height:u32,dpi:Option<u32>)->Option<Vec<u8>> {
    if width==0 || height==0 || width>16384 || height>16384 {return None;}
    let mut body=Vec::new();crate::control_wire::int(1,1,&mut body); // REASON_MANUAL
    let mut resolution=Vec::new();crate::control_wire::int(1,width.into(),&mut resolution);crate::control_wire::int(2,height.into(),&mut resolution);
    crate::control_wire::bytes(2,&resolution,&mut body);
    if let Some(dpi)=dpi.filter(|v|*v>0&&*v<=1000){crate::control_wire::int(3,dpi.into(),&mut body);}
    Some(request(seq,26,&body))
}
pub fn quit_exclusive(seq:u64)->Vec<u8> {request(seq,14,&[])}
/// Display RPC results are independent of capture-setting acknowledgement.
pub fn response(data:&[u8])->Option<(u64,u32,i32)> {
    let outer=decode_wire(data).ok()?;
    if let Some(body)=message(&outer,19) {
        let fields=decode_wire(body).ok()?;
        return (number(&fields,2)==8).then(||(number(&fields,1),19,number(&fields,3) as i32));
    }
    let rpc=decode_wire(message(&outer,22)?).ok()?;
    let header=decode_wire(message(&rpc,1)?).ok()?;
    let id=number(&header,1);
    [10,11,22].into_iter().find_map(|case| {
        let result=decode_wire(message(&rpc,case)?).ok()?;
        Some((id,case,number(&result,1) as i32))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_wire::{bytes, int};
    #[test]
    fn capture_change_confirms_single_track_source_without_new_inventory() {
        let mut body=Vec::new();int(2,42,&mut body);
        assert_eq!(desktop_capture_change(&crate::control_wire::envelope(1,8,&body)),Some(42));
        int(1,1,&mut body); // A captured window is not a desktop source.
        assert_eq!(desktop_capture_change(&crate::control_wire::envelope(2,8,&body)),None);
        assert_eq!(desktop_capture_change(&crate::control_wire::envelope(3,8,&[])),Some(0));
    }
    #[test]
    fn startup_prefers_existing_stream_to_an_unbound_current_placeholder() {
        let mut list=Vec::new();
        for (id,track) in [(7,0),(42,u64::MAX)] {
            let mut screen=Vec::new();int(1,id,&mut screen);int(12,track,&mut screen);bytes(1,&screen,&mut list);
        }
        int(2,42,&mut list);
        let mut screens=screen_sources(&crate::control_wire::envelope(1,7,&list)).unwrap();
        assert_eq!(screens.selected().unwrap().id,42);
        assert_eq!(screens.initial_screen().unwrap().id,7);
        screens.screens[0].video_track_index=-1;
        assert_eq!(screens.initial_screen().unwrap().id,42);
    }
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
    #[test]
    fn display_metadata_keeps_names_primary_and_real_ids() {
        let mut screen=Vec::new();int(1,42,&mut screen);int(7,1,&mut screen);
        int(12,3,&mut screen);bytes(10,"Zako HDR".as_bytes(),&mut screen);
        let mut inventory=Vec::new();bytes(1,&screen,&mut inventory);int(2,42,&mut inventory);
        let parsed=screen_sources(&crate::control_wire::envelope(1,7,&inventory)).unwrap();
        let screen=parsed.selected().unwrap();assert_eq!(screen.display_name,"Zako HDR");assert!(screen.primary);assert_eq!(screen.video_track_index,3);
    }
    #[test]
    fn explicit_display_requests_and_responses_use_official_rpc_cases() {
        for (request,case,body) in [(send_video_track(9001,3),15,vec![(1,WireValue::Varint(3))]),(quit_exclusive(9003),14,vec![])] {
            let fields=decode_wire(&request).unwrap();let rpc=decode_wire(message(&fields,21).unwrap()).unwrap();
            assert_eq!(decode_wire(message(&rpc,case).unwrap()).unwrap(),body);
        }
        for (case,code) in [(10,0),(11,7),(22,0)] {
            let mut header=Vec::new();int(1,9001,&mut header);
            let mut body=Vec::new();int(1,code,&mut body);
            let mut rpc=Vec::new();bytes(1,&header,&mut rpc);bytes(case,&body,&mut rpc);
            assert_eq!(response(&crate::control_wire::envelope(4,22,&rpc)),Some((9001,case,code as i32)));
        }
        assert!(response(&send_video_track(1,0)).is_none());
    }
    #[test]
    fn current_capture_is_bound_without_broadcast_or_resolution_changes() {
        use crate::control_wire::*;
        let request=capture_setting_for_screen(1000,3,CaptureFrameRate::new(120).unwrap(),6,Some(18000000),CaptureResolution::Unchanged,false,-1);
        let resolved=bind_current_capture(&request,42).unwrap();
        let outer=decode_wire(&resolved).unwrap();let rpc=decode_wire(message(&outer,21).unwrap()).unwrap();let settings=decode_wire(message(&rpc,2).unwrap()).unwrap();
        assert_eq!(number(&settings,4),42);assert_eq!(number(&settings,10),3);assert_eq!(number(&settings,18),120);assert_eq!(number(&settings,8),18000000);
        assert!(bind_current_capture(&resolved,7).is_none());
        assert_eq!(DisplayMode::default(),DisplayMode::Preserve);
    }

    #[test]
    fn exclusive_request_requires_measured_dimensions_and_manual_reason() {
        assert!(enter_exclusive(1,0,0,None).is_none());
        let data=enter_exclusive(900,2560,1440,Some(125)).unwrap();
        let outer=decode_wire(&data).unwrap();let rpc=decode_wire(message(&outer,21).unwrap()).unwrap();let body=decode_wire(message(&rpc,26).unwrap()).unwrap();
        assert_eq!(number(&body,1),1);assert_eq!(number(&body,3),125);
        let resolution=decode_wire(message(&body,2).unwrap()).unwrap();assert_eq!(number(&resolution,1),2560);assert_eq!(number(&resolution,2),1440);
    }

    #[test]
    fn unallocated_sources_keep_negative_track_and_activate_without_layout_changes() {
        let mut screen=Vec::new();int(1,42,&mut screen);int(12,u64::MAX,&mut screen);
        let mut list=Vec::new();bytes(1,&screen,&mut list);int(2,42,&mut list);
        let screens=screen_sources(&crate::control_wire::envelope(1,7,&list)).unwrap();
        assert_eq!(screens.selected().unwrap().video_track_index,-1);
        assert_eq!(screens.selected().unwrap().assigned_track(),None);
        assert!(screens.for_track(u32::MAX).is_none());
        let request=activate_screen(10001,42);let outer=decode_wire(&request).unwrap();
        let body=decode_wire(message(&outer,9).unwrap()).unwrap();
        assert_eq!(body,vec![(1,WireValue::Varint(3)),(5,WireValue::Varint(42)),(8,WireValue::Varint(8))]);
        let mut result=Vec::new();int(1,10001,&mut result);int(2,8,&mut result);
        assert_eq!(response(&crate::control_wire::envelope(2,19,&result)),Some((10001,19,0)));
    }

}
