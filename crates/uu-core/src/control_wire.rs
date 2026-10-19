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
/// 5  width           -1 unchanged, or explicit output width
/// 6  height          -1 unchanged, or explicit output height
/// 7  constant        1
/// 8  custom bitrate  bits/second (only for quality 6)
/// 10 resolution type 1=unchanged, 3=follow remote, 4=explicit
/// 12 constant        4
/// 13 constant        2
/// 14 max width       3840
/// 15 max height      2160
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureResolution {
    Unchanged,
    Native,
    Exact(u32, u32),
}
impl CaptureResolution {
    pub fn from_env() -> Self {
        let value = std::env::var("UU_RESOLUTION").unwrap_or_default();
        match value
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        {
            Some((w, h))
                if (320..=3840).contains(&w)
                    && (240..=2160).contains(&h)
                    && w % 2 == 0
                    && h % 2 == 0 =>
            {
                Self::Exact(w, h)
            }
            _ => Self::Native,
        }
    }
}

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
    if cursor_capture {int(3,1,&mut setting);}
    int(1, frame_rate.option(), &mut setting);
    int(2, quality, &mut setting);
    int(4, screen_id as i64 as u64, &mut setting);
    // Runtime switch sentinels observed from the official client.  protobuf
    // int64 negatives use the full 10-byte two's-complement varint.
    match resolution {
        CaptureResolution::Exact(width, height) => {
            int(5, width as u64, &mut setting);
            int(6, height as u64, &mut setting);
            int(16, width as u64, &mut setting);
            int(17, height as u64, &mut setting);
        }
        _ => {
            int(5, u64::MAX, &mut setting);
            int(6, u64::MAX, &mut setting);
        }
    }
    int(7, 1, &mut setting);
    if let Some(bps) = custom_bitrate_bps {
        int(8, bps, &mut setting);
    }
    int(
        10,
        match resolution { CaptureResolution::Native => 3, CaptureResolution::Exact(_, _) => 4, _ => 1 },
        &mut setting,
    );
    int(12, 4, &mut setting);
    int(13, 2, &mut setting);
    int(14, 3840, &mut setting);
    int(15, 2160, &mut setting);
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

/// Current UNIX time in milliseconds (the outer Message field 2).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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

/// Bind the initial current-source sentinel to the host's measured screen ID.
/// Leave legacy broadcasts and explicitly selected sources byte-for-byte intact.
pub fn bind_current_capture(data:&[u8],screen_id:i32)->Option<Vec<u8>> {
    fn replace(data:&[u8],field:u32,new:WireValue<'_>)->Option<Vec<u8>> {
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
    let outer=decode_wire(data).ok()?;
    let payload=|fields:&[(u32,WireValue<'_>)],field|fields.iter().find_map(|(n,v)|match v {WireValue::Bytes(b) if *n==field=>Some(b.to_vec()),_=>None});
    let rpc=payload(&outer,21)?;
    let setting=payload(&decode_wire(&rpc).ok()?,2)?;
    if !decode_wire(&setting).ok()?.contains(&(4,WireValue::Varint(u64::MAX))){return None;}
    let setting=replace(&setting,4,WireValue::Varint(screen_id as i64 as u64))?;
    let rpc=replace(&rpc,2,WireValue::Bytes(&setting))?;
    replace(data,21,WireValue::Bytes(&rpc))
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
    fn native_and_explicit_resolution_requests_do_not_impose_the_old_1080_cap() {
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
        let native = setting(CaptureResolution::Native);
        let fields = decode_wire(&native).unwrap();
        assert!(fields.contains(&(10, WireValue::Varint(3))));
        assert!(!fields.iter().any(|(n, _)| *n == 8));
        let full = setting(CaptureResolution::Exact(3840, 2160));
        let fields = decode_wire(&full).unwrap();
        for (n, value) in [(4, u64::MAX - 1), (5, 3840), (6, 2160), (16, 3840), (17, 2160)] {
            assert!(fields.contains(&(n, WireValue::Varint(value))));
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
            assert_eq!(fields.contains(&(3,WireValue::Varint(1))),capture);
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
        assert_eq!(
            hex::encode(setting),
            "0802100620feffffffffffffffff0128ffffffffffffffffff0130ffffffffffffffffff01380140c0f79c1a50016004680270801e78f01090013c"
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
