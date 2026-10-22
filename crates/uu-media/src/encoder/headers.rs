//! Cache parameter sets from our encoder so every recovery keyframe can be
//! decoded independently. Never fabricate headers or patch an arbitrary peer.
use crate::EncodedFrame;
use uu_proto::media::VideoCodec;
const MAX_HEADER: usize = 64 * 1024;

use crate::{bitstream::annexb_nals as nal_units,av1::obus};
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
