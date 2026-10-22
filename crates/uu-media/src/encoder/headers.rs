//! Cache parameter sets from our encoder so every recovery keyframe can be
//! decoded independently. Never fabricate headers or patch an arbitrary peer.
use crate::EncodedFrame;
use uu_proto::media::VideoCodec;
const MAX_HEADER: usize = 64 * 1024;

pub(crate) fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut positions = Vec::new();
    let mut offset = 0;
    while offset + 3 <= data.len() {
        let length = if data[offset..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[offset..].starts_with(&[0, 0, 1]) {
            3
        } else {
            offset += 1;
            continue;
        };
        if positions.len()>=4096{return Vec::new();}
        positions.push((offset, offset + length));
        offset += length;
    }
    positions
        .iter()
        .enumerate()
        .filter_map(|(i, (_, body))| {
            let end = positions.get(i + 1).map_or(data.len(), |(start, _)| *start);
            (*body < end).then(|| &data[*body..end])
        })
        .collect()
}
/// Low-overhead AV1 OBUs with explicit size fields, as produced by VPL.
pub(crate) fn obus(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut units = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let begin = at;
        let header = data[at];
        at += 1;
        if header & 0x81 != 0 || header & 2 == 0 {
            return Err("invalid AV1 OBU header/size field".into());
        }
        if header & 4 != 0 {
            if at >= data.len() {
                return Err("truncated AV1 extension".into());
            }
            at += 1;
        }
        let mut size = 0usize;
        let mut done = false;
        for index in 0..8 {
            let byte = *data.get(at).ok_or("truncated AV1 OBU length")?;
            at += 1;
            let part = usize::from(byte & 0x7f)
                .checked_shl(index * 7)
                .ok_or("AV1 length overflow")?;
            size = size.checked_add(part).ok_or("AV1 length overflow")?;
            if byte & 0x80 == 0 {
                done = true;
                break;
            }
        }
        if !done {
            return Err("oversized AV1 OBU length".into());
        }
        at = at
            .checked_add(size)
            .filter(|end| *end <= data.len())
            .ok_or("truncated AV1 OBU")?;
        units.push(&data[begin..at]);
        if units.len() > 4096 {
            return Err("too many AV1 OBUs".into());
        }
    }
    Ok(units)
}
#[derive(Default)]
pub(super) struct Headers {
    sets: [Option<Vec<u8>>; 3],
}
impl Headers {
    pub fn prepare(&mut self, codec: VideoCodec, frame: &mut EncodedFrame) -> Result<(), String> {
        let mut present = [false; 3];
        if codec == VideoCodec::Av1 {
            for unit in obus(&frame.data)? {
                if (unit[0] >> 3) & 15 == 1 {
                    if unit.len() > MAX_HEADER {
                        return Err("oversized AV1 sequence header".into());
                    }
                    self.sets[0] = Some(unit.to_vec());
                    present[0] = true;
                }
            }
            if frame.is_keyframe && !present[0] {
                let header = self.sets[0]
                    .as_ref()
                    .ok_or("AV1 recovery frame has no sequence header")?;
                let mut output = Vec::with_capacity(header.len() + frame.data.len());
                let units = obus(&frame.data)?;
                let delimiter = units
                    .first()
                    .filter(|u| (u[0] >> 3) & 15 == 2)
                    .map_or(0, |u| u.len());
                output.extend_from_slice(&frame.data[..delimiter]);
                output.extend_from_slice(header);
                output.extend_from_slice(&frame.data[delimiter..]);
                frame.data = output.into();
            }
            return Ok(());
        }
        for nal in nal_units(&frame.data) {
            let slot = if codec == VideoCodec::H264 {
                match nal[0] & 31 {
                    7 => Some(0),
                    8 => Some(1),
                    _ => None,
                }
            } else {
                match (nal[0] >> 1) & 63 {
                    32 => Some(0),
                    33 => Some(1),
                    34 => Some(2),
                    _ => None,
                }
            };
            if let Some(slot) = slot {
                if nal.len() > MAX_HEADER {
                    return Err("oversized encoder parameter set".into());
                }
                self.sets[slot] = Some(nal.to_vec());
                present[slot] = true;
            }
        }
        if frame.is_keyframe {
            let count = if codec == VideoCodec::H264 { 2 } else { 3 };
            let mut output = Vec::new();
            for index in 0..count {
                let header = self.sets[index]
                    .as_ref()
                    .ok_or("recovery frame lacks encoder parameter sets")?;
                // Prefix the complete ordered chain if any set is absent. A
                // lone missing PPS must not precede the only SPS in the AU.
                if present[..count].iter().any(|p| !*p) {
                    output.extend_from_slice(&[0, 0, 0, 1]);
                    output.extend_from_slice(header);
                }
            }
            if !output.is_empty() {
                output.extend_from_slice(&frame.data);
                frame.data = output.into();
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;
    #[test]
    fn recovery_uses_actual_headers_and_av1_obu_lengths_are_checked() {
        let mut headers = Headers::default();
        let mut key = EncodedFrame {
            data: vec![
                0, 0, 1, 64, 1, 4, 0, 0, 1, 66, 1, 5, 0, 0, 1, 68, 1, 6, 0, 0, 1, 38, 1, 7,
            ]
            .into(),
            pts: Timestamp(0),
            is_keyframe: true,
        };
        headers.prepare(VideoCodec::Hevc, &mut key).unwrap();
        key.data = vec![0, 0, 1, 38, 1, 8].into();
        headers.prepare(VideoCodec::Hevc, &mut key).unwrap();
        assert_eq!(nal_units(&key.data).len(), 4);
        assert_eq!(nal_units(&key.data)[0], &[64, 1, 4]);
        let mut av1 = Headers::default();
        key.data = vec![0x12, 0, 0x0a, 1, 7, 0x32, 1, 8].into();
        av1.prepare(VideoCodec::Av1, &mut key).unwrap();
        key.data = vec![0x12, 0, 0x32, 1, 9].into();
        av1.prepare(VideoCodec::Av1, &mut key).unwrap();
        assert_eq!(&*key.data, &[0x12, 0, 0x0a, 1, 7, 0x32, 1, 9]);
        assert!(obus(&[0x0a, 0xff]).is_err());
        assert!(obus(&[0x08, 1]).is_err());
        assert!(obus(&[0x0a, 5, 1]).is_err());
    }
}
