//! Minimal protobuf wire helpers for the UU CONTROL data channel.
//!
//! The official controller and host exchange `Message { 1: seq, 2: timestamp,
//! <kind>: payload }` protobufs on the `CONTROL_DATA_CHANNEL` data channel.
//! The handshake is a SimpleAction echo request/response (kind 3) that the host
//! repeats until answered; only after that does it accept input JSON.
//!
//! Recovered from the interoperable `uu-remote-linux` project's viewer wire
//! definitions (viewer/wire.rs).

#[derive(Debug, PartialEq, Eq)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

pub fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
}

pub fn int(field: u32, value: u64, out: &mut Vec<u8>) {
    varint(u64::from(field) << 3, out);
    varint(value, out);
}

pub fn bytes(field: u32, value: &[u8], out: &mut Vec<u8>) {
    varint((u64::from(field) << 3) | 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

/// `Message { 1: seq, <kind>: payload }`.
pub fn envelope(seq: u64, kind: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    int(1, seq, &mut out);
    bytes(kind, payload, &mut out);
    out
}

/// Viewer FeatureFlag pairs; clipboard (field 8) is advertised separately.
pub const VIEWER_FEATURES: [[u32; 2]; 8] = [
    [1, 6],
    [2, 2],
    [3, 1],
    [4, 2],
    [6, 2],
    [7, 2],
    [8, 3],
    [9, 1],
];

fn feature_flag(flags: &[[u32; 2]]) -> Vec<u8> {
    let mut out = Vec::new();
    for [f, v] in flags {
        int(*f, u64::from(*v), &mut out);
    }
    out
}

fn session_features(clipboard: bool) -> Vec<u8> {
    let mut flags: Vec<[u32; 2]> = VIEWER_FEATURES
        .iter()
        .copied()
        .filter(|p| p[0] != 8)
        .collect();
    if clipboard {
        flags.push([8, 3]);
    }
    feature_flag(&flags)
}

/// Requested rate, not a measured receive/decode/render rate. Official Windows
/// descriptors define only FPS_UNKNOWN/30/60/90/144; arbitrary counts use fpsCount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureFrameRate(u32);
impl Default for CaptureFrameRate { fn default() -> Self { Self(60) } }
impl CaptureFrameRate {
    pub const MIN: u32 = 5;
    pub const MAX: u32 = 144;
    pub fn new(fps: u32) -> Option<Self> {
        (Self::MIN..=Self::MAX).contains(&fps).then_some(Self(fps))
    }
    pub fn fps(self) -> u32 { self.0 }
    pub fn option(self) -> u64 {
        match self.0 { 30 => 1, 60 => 2, 90 => 3, 144 => 4, _ => 0 }
    }
    pub fn parse(value: &str) -> Option<Self> {
        // Keep old UU_FPS=1/2/3/4 scripts compatible; >=5 is a literal rate.
        let value = value.parse::<u32>().ok()?;
        Self::new(match value { 1 => 30, 2 => 60, 3 => 90, 4 => 144, other => other })
    }
    pub fn from_env() -> Self {
        std::env::var("UU_FPS").ok().as_deref().and_then(Self::parse).unwrap_or_default()
    }
}

/// CaptureSettingRequest (CONTROL Message 21): ask the controlled end to switch
/// frame rate / quality / bitrate.
///
/// Recovered from an official Windows controller capture.  The inner setting
/// is much wider than the Linux interoperable viewer's definition:
///
/// ```text
/// 1  fps option      1=30, 2=60, 3=90, 4=144
/// 2  quality preset  1 fast, 2 general, 3 hd, 4 bluray, 5 auto, 6 custom
/// 4  screen id       -2 broadcasts to existing tracks; nonnegative selects one source
/// 5  desktop width   -1 leaves the desktop size unchanged
/// 6  desktop height  -1 leaves the desktop size unchanged
/// 7  constant        1
/// 8  custom bitrate  bits/second (only for quality 6)
/// 10 resolution type 1=default, 3=follow remote, 4=explicit desktop
/// 12 constant        4
/// 13 constant        2
/// 14 max scale width encoded output bound (default 3840)
/// 15 max scale height encoded output bound (default 2160)
/// 18 explicit fps    actual integer count, including 120/custom
/// ```
///
/// The outer `Message` carries `1: seq, 2: unix_ms, 21: Rpc{1:{1:seq},2:setting}`.
pub fn capture_setting_request(
    seq: u64,
    timestamp_ms: u64,
    fps_option: u64,
    quality: u64,
    custom_bitrate_bps: Option<u64>,
) -> Vec<u8> {
    capture_setting_request_with_resolution(
        seq,
        timestamp_ms,
        fps_option,
        quality,
        custom_bitrate_bps,
        CaptureResolution::Unchanged,
    )
}

pub use uu_core::video_options::CaptureResolution;

pub fn capture_setting_request_with_resolution(
    seq: u64,
    timestamp_ms: u64,
    fps_option: u64,
    quality: u64,
    custom_bitrate_bps: Option<u64>,
    resolution: CaptureResolution,
) -> Vec<u8> {
    let frame_rate = CaptureFrameRate::parse(&fps_option.to_string()).unwrap_or_default();
    capture_setting_request_at_fps(seq, timestamp_ms, frame_rate, quality, custom_bitrate_bps, resolution)
}

pub fn capture_setting_request_at_fps(
    seq: u64, timestamp_ms: u64, frame_rate: CaptureFrameRate,
    quality: u64, custom_bitrate_bps: Option<u64>, resolution: CaptureResolution,
) -> Vec<u8> {
    capture_setting_request_with_cursor(seq,timestamp_ms,frame_rate,quality,custom_bitrate_bps,resolution,false)
}

pub fn capture_setting_request_with_cursor(
    seq:u64,timestamp_ms:u64,frame_rate:CaptureFrameRate,quality:u64,
    custom_bitrate_bps:Option<u64>,resolution:CaptureResolution,cursor_capture:bool,
)->Vec<u8> {
    capture_setting_for_screen(seq,timestamp_ms,frame_rate,quality,custom_bitrate_bps,resolution,cursor_capture,-2)
}

/// A nonnegative id selects an advertised source. -1 addresses the existing
/// capture in a single-track session; -2 broadcasts to existing tracks and is
/// retained only by the legacy wire-compatible wrapper above.
pub fn capture_setting_for_screen(
    seq:u64,timestamp_ms:u64,frame_rate:CaptureFrameRate,quality:u64,
    custom_bitrate_bps:Option<u64>,resolution:CaptureResolution,cursor_capture:bool,screen_id:i32,
)->Vec<u8> {
    let mut setting = Vec::new();
    // A capture-setting RPC changes an existing capture. Send false explicitly
    // as well, so returning to a local cursor cannot leave host compositing on.
    int(3, u64::from(cursor_capture), &mut setting);
    int(1, frame_rate.option(), &mut setting);
    int(2, quality, &mut setting);
    int(4, screen_id as i64 as u64, &mut setting);
    // Desktop size and encoder output are independent. Never populate either
    // desktop size pair (5/6 or 16/17) with an output preset.
    int(5, u64::MAX, &mut setting);
    int(6, u64::MAX, &mut setting);
    int(7, 1, &mut setting);
    if let Some(bps) = custom_bitrate_bps {
        int(8, bps, &mut setting);
    }
    // DEFAULT can restore host-saved desktop settings. FOLLOW_REMOTE is the
    // explicit display policy for every output preset, including auto/native.
    int(10, 3, &mut setting);
    int(12, 4, &mut setting);
    int(13, 2, &mut setting);
    let (width, height) = resolution.output_bounds();
    // Official Windows CaptureSettingRequest.max_scale_width/height feed the
    // video capture/encoder settings, independently of desktop resolution.
    int(14, u64::from(width), &mut setting);
    int(15, u64::from(height), &mut setting);
    int(18, frame_rate.fps() as u64, &mut setting);

    let mut header = Vec::new();
    int(1, seq, &mut header);
    let mut rpc = Vec::new();
    bytes(1, &header, &mut rpc);
    bytes(2, &setting, &mut rpc);

    let mut out = Vec::new();
    int(1, seq, &mut out);
    int(2, timestamp_ms, &mut out);
    bytes(21, &rpc, &mut out);
    out
}

/// Numeric capture-setting metadata only; never logs generic CONTROL payloads.
pub(crate) fn capture_setting_metadata(data: &[u8]) -> Option<serde_json::Value> {
    fn number(fields: &[(u32, WireValue<'_>)], field: u32) -> Option<u64> {
        fields.iter().find_map(|(n, value)| match value {
            WireValue::Varint(value) if *n == field => Some(*value), _ => None,
        })
    }
    fn nested<'a>(fields: &[(u32, WireValue<'a>)], field: u32) -> Option<&'a [u8]> {
        fields.iter().find_map(|(n, value)| match value {
            WireValue::Bytes(value) if *n == field => Some(*value), _ => None,
        })
    }
    let envelope = decode_wire(data).ok()?;
    let rpc = decode_wire(nested(&envelope, 21)?).ok()?;
    let setting = decode_wire(nested(&rpc, 2)?).ok()?;
    let header = decode_wire(nested(&rpc, 1)?).ok()?;
    let fields: std::collections::BTreeMap<_, _> = setting.into_iter().filter_map(|(n, value)| match value {
        WireValue::Varint(value) if (1..=18).contains(&n) => Some((n.to_string(), value)), _ => None,
    }).collect();
    Some(serde_json::json!({"event":"capture_setting_send", "seq":number(&envelope,1),
        "timestamp":number(&envelope,2), "request":number(&header,1), "fields":fields}))
}

pub use uu_core::time::now_ms;

/// `Message { 1: seq, 3: SimpleAction { 1: 0, 4: features } }`.
pub fn echo_request(seq: u64) -> Vec<u8> {echo_request_with_clipboard(seq,false)}
pub fn echo_request_with_clipboard(seq:u64,clipboard:bool)->Vec<u8> {
    let mut action = Vec::new();
    int(1, 0, &mut action);
    bytes(4, &session_features(clipboard), &mut action);
    envelope(seq, 3, &action)
}

/// `Message { 1: seq, 3: SimpleAction { 1: 1, 2: {"seq": request_seq}, 4: features } }`.
pub fn echo_response(seq:u64,request_seq:u64)->Vec<u8>{echo_response_with_clipboard(seq,request_seq,false)}
pub fn echo_response_with_clipboard(seq:u64,request_seq:u64,clipboard:bool)->Vec<u8> {
    let mut action = Vec::new();
    int(1, 1, &mut action);
    bytes(
        2,
        format!("{{\"seq\":{request_seq}}}").as_bytes(),
        &mut action,
    );
    bytes(4, &session_features(clipboard), &mut action);
    envelope(seq, 3, &action)
}

pub fn echo_clipboard_supported(data:&[u8])->bool {
    let Ok(fields)=decode_wire(data) else{return false;};
    fields.iter().any(|(n,v)|match (n,v){(3,WireValue::Bytes(action))=>decode_wire(action).is_ok_and(|fields|
        fields.iter().any(|(n,v)|match (n,v){(4,WireValue::Bytes(flags))=>decode_wire(flags).is_ok_and(|fields|
            fields.iter().any(|(n,v)|matches!((n,v),(8,WireValue::Varint(3..))))),_=>false})),_=>false})
}

/// Returns the request seq when `data` is a host echo request (SimpleAction
/// with action 0, omitted field 1 included).
pub fn echo_request_seq(data: &[u8]) -> Option<u64> {
    let fields = decode_wire(data).ok()?;
    let seq = fields.iter().find_map(|(n, v)| match (n, v) {
        (1, WireValue::Varint(v)) => Some(*v),
        _ => None,
    })?;
    fields.iter().find_map(|(n, v)| match (n, v) {
        (3, WireValue::Bytes(action)) => {
            let af = decode_wire(action).ok()?;
            let a = af
                .iter()
                .find_map(|(n, v)| match (n, v) {
                    (1, WireValue::Varint(v)) => Some(*v),
                    _ => None,
                })
                .unwrap_or(0);
            (a == 0).then_some(seq)
        }
        _ => None,
    })
}

/// True when `data` is an echo response acknowledging the request `seq`.
pub fn echo_matches(data: &[u8], seq: u64) -> bool {
    let Ok(fields) = decode_wire(data) else {
        return false;
    };
    fields.iter().any(|(n, v)| match (n, v) {
        (3, WireValue::Bytes(action)) => {
            let Ok(af) = decode_wire(action) else {
                return false;
            };
            let response = af
                .iter()
                .any(|(n, v)| *n == 1 && *v == WireValue::Varint(1));
            response
                && af.iter().any(|(n, v)| match (n, v) {
                    (2, WireValue::Bytes(args)) => {
                        serde_json::from_slice::<serde_json::Value>(args)
                            .ok()
                            .and_then(|j| j.get("seq").and_then(|s| s.as_u64()))
                            == Some(seq)
                    }
                    _ => false,
                })
        }
        _ => false,
    })
}

/// Capture-setting RPC response (22), matched by RequestHeader.request_id.
/// An empty CaptureSettingResponse means success; nonzero errors mean reject.
pub fn capture_setting_response(data: &[u8]) -> Option<(u64, bool)> {
    let outer = decode_wire(data).ok()?;
    let rpc = outer.iter().find_map(|(n, v)| match (n, v) {
        (22, WireValue::Bytes(b)) => Some(*b), _ => None,
    })?;
    let rpc = decode_wire(rpc).ok()?;
    let header = rpc.iter().find_map(|(n, v)| match (n, v) {
        (1, WireValue::Bytes(b)) => Some(*b), _ => None,
    })?;
    let id = decode_wire(header).ok()?.iter().find_map(|(n, v)| match (n, v) {
        (1, WireValue::Varint(id)) => Some(*id), _ => None,
    })?;
    let response = rpc.iter().find_map(|(n, v)| match (n, v) {
        (2, WireValue::Bytes(b)) => Some(*b), _ => None,
    })?;
    let errors = decode_wire(response).ok()?;
    // CaptureSettingResponse.errors is repeated Error, not a packed integer
    // array. Success entries can carry descriptive text with error_code=0.
    let mut rejected = false;
    for (field, value) in errors {
        if field != 1 { continue; }
        let WireValue::Bytes(error) = value else { return None; };
        let error = decode_wire(error).ok()?;
        for (field, value) in error {
            if field == 1 {
                let WireValue::Varint(code) = value else { return None; };
                rejected |= code != 0;
            }
        }
    }
    Some((id, !rejected))
}

fn replace_wire_field(data:&[u8],field:u32,new:WireValue<'_>)->Option<Vec<u8>> {
        let mut out=Vec::new();let mut found=false;
        for (n,value) in decode_wire(data).ok()? {
            let value=if n==field {found=true;&new}else{&value};
            match value {
                WireValue::Varint(v)=>int(n,*v,&mut out),
                WireValue::Bytes(v)=>bytes(n,v,&mut out),
                WireValue::Fixed64(v)=>{varint((n as u64)<<3|1,&mut out);out.extend(v);},
                WireValue::Fixed32(v)=>{varint((n as u64)<<3|5,&mut out);out.extend(v);},
            }
        }
        found.then_some(out)
    }

/// Bind the initial current-source sentinel to the host's measured screen ID.
/// Leave legacy broadcasts and explicitly selected sources byte-for-byte intact.
pub fn bind_current_capture(data:&[u8],screen_id:i32)->Option<Vec<u8>> {
    let outer=decode_wire(data).ok()?;
    let payload=|fields:&[(u32,WireValue<'_>)],field|fields.iter().find_map(|(n,v)|match v {WireValue::Bytes(b) if *n==field=>Some(b.to_vec()),_=>None});
    let rpc=payload(&outer,21)?;
    let setting=payload(&decode_wire(&rpc).ok()?,2)?;
    if !decode_wire(&setting).ok()?.contains(&(4,WireValue::Varint(u64::MAX))){return None;}
    let setting=replace_wire_field(&setting,4,WireValue::Varint(screen_id as i64 as u64))?;
    let rpc=replace_wire_field(&rpc,2,WireValue::Bytes(&setting))?;
    replace_wire_field(data,21,WireValue::Bytes(&rpc))
}

/// Apply the session's verified codec at the common CONTROL boundary. Legacy
/// request builders use the official HEVC preference, even on H.264-only peers.
pub(crate) fn capture_codec(data: &[u8], hevc: bool) -> Option<Vec<u8>> {
    fn child(data: &[u8], field: u32) -> Option<&[u8]> {
        decode_wire(data).ok()?.into_iter().find_map(|(n,v)| match v {
            WireValue::Bytes(b) if n == field => Some(b), _ => None,
        })
    }
    let rpc = child(data, 21)?;
    let setting = child(rpc, 2)?;
    let setting = replace_wire_field(setting, 13, WireValue::Varint(if hevc { 2 } else { 1 }))?;
    let rpc = replace_wire_field(rpc, 2, WireValue::Bytes(&setting))?;
    replace_wire_field(data, 21, WireValue::Bytes(&rpc))
}

/// Decode a protobuf message into `(field number, value)` pairs. Nested byte
/// payloads are left opaque.
pub fn decode_wire(input: &[u8]) -> Result<Vec<(u32, WireValue<'_>)>, ()> {
    fn take<'a>(input: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], ()> {
        let end = offset.checked_add(len).ok_or(())?;
        let v = input.get(*offset..end).ok_or(())?;
        *offset = end;
        Ok(v)
    }
    fn read_varint(input: &[u8], offset: &mut usize) -> Result<u64, ()> {
        let mut result = 0u64;
        for shift in (0..70).step_by(7) {
            let b = *input.get(*offset).ok_or(())?;
            *offset += 1;
            if shift == 63 && b > 1 {
                return Err(());
            }
            result |= u64::from(b & 127) << shift;
            if b & 128 == 0 {
                return Ok(result);
            }
        }
        Err(())
    }
    let mut offset = 0usize;
    let mut fields = Vec::new();
    while offset < input.len() {
        let key = read_varint(input, &mut offset)?;
        let number = (key >> 3) as u32;
        if number == 0 {
            return Err(());
        }
        let value = match (key & 7) as u8 {
            0 => WireValue::Varint(read_varint(input, &mut offset)?),
            1 => WireValue::Fixed64(take(input, &mut offset, 8)?.try_into().map_err(|_| ())?),
            2 => {
                let len = usize::try_from(read_varint(input, &mut offset)?).map_err(|_| ())?;
                WireValue::Bytes(take(input, &mut offset, len)?)
            }
            5 => WireValue::Fixed32(take(input, &mut offset, 4)?.try_into().map_err(|_| ())?),
            _ => return Err(()),
        };
        fields.push((number, value));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codec_boundary_changes_only_codec_and_passes_other_control_messages() {
        let request=capture_setting_for_screen(7,123,CaptureFrameRate::new(120).unwrap(),6,Some(30_000_000),
            CaptureResolution::Exact(3840,2160),true,5);
        let before=capture_setting_metadata(&request).unwrap();
        for hevc in [false,true] {
            let changed=capture_codec(&request,hevc).unwrap();
            let mut expected=before.clone();
            expected["fields"]["13"]=serde_json::json!(if hevc {2}else{1});
            assert_eq!(capture_setting_metadata(&changed).unwrap(),expected);
        }
        assert!(capture_codec(&echo_request(3),true).is_none());
        assert!(capture_codec(br#"{"type":1}"#,true).is_none());
    }

    #[test]
    fn output_presets_change_encoder_bounds_while_every_request_preserves_the_desktop() {
        fn setting(resolution: CaptureResolution) -> Vec<u8> {
            let msg = capture_setting_request_with_resolution(1, 1, 2, 5, None, resolution);
            let rpc = decode_wire(&msg)
                .unwrap()
                .into_iter()
                .find_map(|(n, v)| match (n, v) {
                    (21, WireValue::Bytes(b)) => Some(b.to_vec()),
                    _ => None,
                })
                .unwrap();
            decode_wire(&rpc)
                .unwrap()
                .into_iter()
                .find_map(|(n, v)| match (n, v) {
                    (2, WireValue::Bytes(b)) => Some(b.to_vec()),
                    _ => None,
                })
                .unwrap()
        }
        for (resolution, width, height) in [
            (CaptureResolution::Unchanged, 3840, 2160),
            (CaptureResolution::Native, 3840, 2160),
            (CaptureResolution::Exact(1920, 1080), 1920, 1080),
            (CaptureResolution::Exact(2560, 1440), 2560, 1440),
            (CaptureResolution::Exact(3840, 2160), 3840, 2160),
        ] {
            let data = setting(resolution);
            let fields = decode_wire(&data).unwrap();
            for (n, value) in [(4, u64::MAX - 1), (5, u64::MAX), (6, u64::MAX), (10, 3), (14, width), (15, height)] {
                assert!(fields.contains(&(n, WireValue::Varint(value))), "{resolution:?}: field {n}");
            }
            assert!(!fields.iter().any(|(n, _)| matches!(n, 8 | 16 | 17)));
        }
    }
    #[test]
    fn literal_frame_rates_use_count_not_unknown_enum_numbers() {
        for fps in [5, 30, 60, 75, 90, 120, 144] {
            let rate = CaptureFrameRate::new(fps).unwrap();
            let msg = capture_setting_request_at_fps(1, 2, rate, 5, None, CaptureResolution::Native);
            let outer = decode_wire(&msg).unwrap();
            let rpc = outer.iter().find_map(|(n,v)| match (n,v) { (21,WireValue::Bytes(b))=>Some(*b),_=>None }).unwrap();
            let rpc = decode_wire(rpc).unwrap();
            let setting = rpc.iter().find_map(|(n,v)| match (n,v) { (2,WireValue::Bytes(b))=>Some(*b),_=>None }).unwrap();
            let setting = decode_wire(setting).unwrap();
            assert!(setting.contains(&(18,WireValue::Varint(fps as u64))));
            assert!(setting.contains(&(1,WireValue::Varint(rate.option()))));
            if ![30,60,90,144].contains(&fps) { assert_eq!(rate.option(),0); }
        }
        assert_eq!(CaptureFrameRate::parse("2"), CaptureFrameRate::new(60));
        assert_eq!(CaptureFrameRate::parse("120"), CaptureFrameRate::new(120));
        for invalid in ["0", "145", "-1", "no"] { assert_eq!(CaptureFrameRate::parse(invalid),None); }
    }
    #[test]
    fn cursor_capture_switch_preserves_quality_rate_and_screen() {
        for capture in [false,true] {
            let msg=capture_setting_request_with_cursor(7,11,CaptureFrameRate::new(120).unwrap(),6,Some(30_000_000),CaptureResolution::Native,capture);
            let outer=decode_wire(&msg).unwrap();
            let rpc=outer.iter().find_map(|(n,v)|match (n,v){(21,WireValue::Bytes(b))=>Some(*b),_=>None}).unwrap();
            let rpc=decode_wire(rpc).unwrap();
            let setting=rpc.iter().find_map(|(n,v)|match (n,v){(2,WireValue::Bytes(b))=>Some(*b),_=>None}).unwrap();
            let fields=decode_wire(setting).unwrap();
            assert_eq!(fields.iter().filter(|(n,_)|*n==3).count(),1);
            assert!(fields.contains(&(3,WireValue::Varint(u64::from(capture)))));
            for (field,value) in [(18,120),(8,30_000_000),(4,u64::MAX-1)] {assert!(fields.contains(&(field,WireValue::Varint(value))));}
        }
    }
    #[test]
    fn clipboard_capability_is_only_advertised_when_enabled() {
        assert!(!echo_clipboard_supported(&echo_request(1)));
        assert!(!echo_clipboard_supported(&echo_response(2,1)));
        assert!(echo_clipboard_supported(&echo_request_with_clipboard(1,true)));
        assert!(echo_clipboard_supported(&echo_response_with_clipboard(2,1,true)));
        assert!(!echo_clipboard_supported(&[0]));
    }
    #[test]
    fn echo_request_is_detected() {
        let req = echo_request(7);
        assert_eq!(echo_request_seq(&req), Some(7));
        assert!(!echo_matches(&req, 7));
    }

    #[test]
    fn echo_response_acknowledges_request_seq() {
        let resp = echo_response(99, 7);
        assert!(echo_matches(&resp, 7));
        assert!(!echo_matches(&resp, 99));
        assert_eq!(echo_request_seq(&resp), None);
    }

    #[test]
    fn capture_setting_matches_official_custom_bitrate() {
        let msg = capture_setting_request(115, 1_792_263_156_376, 2, 6, Some(55_000_000));
        let fields = decode_wire(&msg).unwrap();
        assert!(fields
            .iter()
            .any(|(n, v)| *n == 1 && *v == WireValue::Varint(115)));
        assert!(fields
            .iter()
            .any(|(n, v)| *n == 2 && *v == WireValue::Varint(1_792_263_156_376)));
        let rpc = fields
            .iter()
            .find_map(|(n, v)| match (n, v) {
                (21, WireValue::Bytes(b)) => Some(*b),
                _ => None,
            })
            .expect("field 21");
        let rf = decode_wire(rpc).unwrap();
        let setting = rf
            .iter()
            .find_map(|(n, v)| match (n, v) {
                (2, WireValue::Bytes(b)) => Some(*b),
                _ => None,
            })
            .expect("rpc field 2");
        // The captured request omitted the proto3 default for cursor_capture.
        // Our runtime request explicitly disables host compositing (18 00)
        // and follows the remote desktop (50 03 instead of captured DEFAULT).
        assert_eq!(
            hex::encode(setting),
            "18000802100620feffffffffffffffff0128ffffffffffffffffff0130ffffffffffffffffff01380140c0f79c1a50036004680270801e78f01090013c"
        );
    }

    #[test]
    fn host_echo_with_omitted_action_is_detected() {
        // Message{1:893,3:{4:<features>}} — action 0 elided by proto3.
        let mut action = Vec::new();
        bytes(4, &[8, 6, 16, 3], &mut action);
        let msg = envelope(893, 3, &action);
        assert_eq!(echo_request_seq(&msg), Some(893));
    }
}

#[cfg(test)]
mod settings_response_tests {
    use super::*;
    #[test]
    fn per_setting_errors_use_error_code_and_reject_malformed_entries() {
        let mut header = Vec::new(); int(1, 2001, &mut header);
        let reply = |errors: &[u8]| {
            let mut rpc = Vec::new(); bytes(1, &header, &mut rpc); bytes(2, errors, &mut rpc);
            capture_setting_response(&envelope(1, 22, &rpc))
        };
        let mut ok = Vec::new(); bytes(2, b"unchanged", &mut ok);
        let mut errors = Vec::new(); bytes(1, &ok, &mut errors);
        assert_eq!(reply(&errors), Some((2001, true)));
        let mut failed = Vec::new(); int(1, u64::MAX, &mut failed);
        bytes(1, &failed, &mut errors);
        assert_eq!(reply(&errors), Some((2001, false)));
        assert_eq!(reply(&[8, 3]), None); // errors must be a message
        assert_eq!(reply(&[10, 1, 128]), None); // truncated nested Error
    }
    #[test]
    fn settings_confirmation_matches_rpc_id_and_rejects_non_capture_responses() {
        let mut header = vec![]; int(1, 2007, &mut header);
        let mut rpc = vec![]; bytes(1, &header, &mut rpc); bytes(2, &[], &mut rpc);
        assert_eq!(capture_setting_response(&envelope(42, 22, &rpc)), Some((2007, true)));
        let mut error = vec![]; int(1, 3, &mut error);
        let mut failed = vec![]; bytes(1, &error, &mut failed);
        let mut rpc = vec![]; bytes(1, &header, &mut rpc); bytes(2, &failed, &mut rpc);
        assert_eq!(capture_setting_response(&envelope(43, 22, &rpc)), Some((2007, false)));
        assert_eq!(capture_setting_response(&echo_response(44, 2007)), None);
        let mut unrelated = vec![]; bytes(1, &header, &mut unrelated); bytes(3, &[], &mut unrelated);
        assert_eq!(capture_setting_response(&envelope(45, 22, &unrelated)), None);
    }
}

#[cfg(test)]
mod capture_diagnostic_tests {
    use super::*;
    #[test]
    fn capture_metadata_retains_wire_identifiers_and_numbers_but_excludes_other_control_data() {
        let msg = capture_setting_for_screen(2001, 123456, CaptureFrameRate::default(),
            6, Some(20_000_000), CaptureResolution::Exact(1920,1080), false, 42);
        let metadata = capture_setting_metadata(&msg).unwrap();
        assert_eq!(metadata["seq"], 2001);
        assert_eq!(metadata["timestamp"], 123456);
        assert_eq!(metadata["request"], 2001);
        assert_eq!(metadata["fields"]["4"], 42);
        assert_eq!(metadata["fields"]["10"], 3);
        assert_eq!(metadata["fields"]["14"], 1920);
        assert_eq!(metadata["fields"]["15"], 1080);
        assert!(metadata["fields"].get("16").is_none());
        assert!(capture_setting_metadata(br#"{"action":"kbd_press","key":65}"#).is_none());
        assert!(capture_setting_metadata(&crate::clipboard::text_change(1,"private fixture").unwrap()).is_none());
        assert!(capture_setting_metadata(&echo_request(1)).is_none());
    }
}
