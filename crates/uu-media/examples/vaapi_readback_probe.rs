//! Compare every visible decoded byte against the driver download on generated
//! H.264 files containing AUDs. Never capture a desktop or open audio/input.
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::time::Instant;
    use uu_media::{EncodedFrame, Timestamp, VideoDecoder};
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: vaapi_readback_probe GENERATED_WITH_AUD.h264"))?;
    let data = std::fs::read(path)?;
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        let prefix = if data[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        if data.get(i + prefix).is_some_and(|header| header & 31 == 9) {
            starts.push(i);
        }
        i += prefix;
    }
    anyhow::ensure!(
        !starts.is_empty(),
        "test file must contain AUDs; use ffmpeg -c:v copy -bsf:v h264_metadata=aud=insert"
    );
    starts[0] = 0;
    starts.push(data.len());
    std::env::set_var("UU_VAAPI_READBACK", "legacy");
    let mut reference = uu_media::hw::vaapi::VaapiDecoder::new()?;
    std::env::remove_var("UU_VAAPI_READBACK");
    let mut direct = uu_media::hw::vaapi::VaapiDecoder::new()?;
    let mut frames = 0;
    let mut times = [0.0_f64; 2];
    for (index, pair) in starts.windows(2).enumerate() {
        let bytes = bytes::Bytes::copy_from_slice(&data[pair[0]..pair[1]]);
        let encoded = EncodedFrame {
            is_keyframe: EncodedFrame::detect_h264_keyframe(&bytes),
            data: bytes,
            pts: Timestamp(index as u64 * 16_667),
        };
        let at = Instant::now();
        let expected = reference
            .decode(&encoded)
            .with_context(|| format!("driver reference at access unit {index}"))?;
        times[0] += at.elapsed().as_secs_f64();
        let at = Instant::now();
        let actual = direct
            .decode(&encoded)
            .with_context(|| format!("DMA-BUF candidate at access unit {index}"))?;
        times[1] += at.elapsed().as_secs_f64();
        match (expected, actual) {
            (Some(a), Some(b)) => {
                anyhow::ensure!(
                    (a.width, a.height, a.stride, a.format, a.pts)
                        == (b.width, b.height, b.stride, b.format, b.pts),
                    "frame {index} metadata mismatch"
                );
                anyhow::ensure!(
                    a.data == b.data,
                    "frame {index} visible decoded pixels differ from driver download"
                );
                frames += 1;
            }
            (None, None) => {}
            _ => anyhow::bail!("frame {index} decoder output ordering mismatch"),
        }
    }
    anyhow::ensure!(frames > 0, "no actual hardware frames decoded");
    println!(
        "{}",
        serde_json::json!({"video":"passed","frames":frames,"pixels_equal":true,"reference_backend":reference.backend_name(),"direct_backend":direct.backend_name(),"legacy_mean_ms":times[0]*1000.0/frames as f64,"direct_mean_ms":times[1]*1000.0/frames as f64,"audio_opened":false,"desktop_captured":false,"input_sent":false})
    );
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("VAAPI readback validation requires Linux");
}
