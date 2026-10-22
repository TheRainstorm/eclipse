//! ABI/ownership contract test against an isolated fake runtime. No GPU is used.
use anyhow::ensure;
use uu_media::{
    encoder::{self, Preference, RateControl, Settings},
    RawFrame, Timestamp,
};
use uu_proto::media::VideoCodec;
fn main() -> anyhow::Result<()> {
    ensure!(
        std::env::var("ECLIPSE_VPL_CONTRACT").as_deref() == Ok("1"),
        "use tools/native/vpl_contract_smoke.py; this probe is not hardware validation"
    );
    let case = std::env::var("ECLIPSE_VPL_CONTRACT_CASE")?;
    let base = Settings {
        codec: VideoCodec::Hevc,
        rate_control: RateControl::Vbr { peak_kbps: 1600 },
        width: 64,
        height: 64,
        fps: 60,
        bitrate_kbps: 800,
        preference: Preference::Hardware,
    };
    if matches!(case.as_str(), "software" | "fps" | "partial" | "init_fps" | "codec" | "mode") {
        ensure!(
            encoder::create(base).is_err(),
            "invalid/partially accelerated runtime must fail"
        );
        ensure!(
            encoder::create(Settings {
                preference: Preference::Auto,
                ..base
            })
            .is_err(),
            "explicit HEVC cannot fall back to H264"
        );
    } else {
        for codec in [VideoCodec::Hevc, VideoCodec::Av1] {
            for rate_control in [RateControl::Cbr, RateControl::Vbr { peak_kbps: 1600 }] {
                let mut encoder = encoder::create(Settings {
                    codec,
                    rate_control,
                    ..base
                })?;
                ensure!(encoder.codec() == codec, "worker lost codec identity");
                for i in 0..2 {
                    let frame = RawFrame::new_i420(
                        64,
                        64,
                        vec![128; 64 * 64 * 3 / 2],
                        Timestamp(i * 16666),
                    );
                    let coded = encoder.encode(&frame);
                    if matches!(case.as_str(), "timestamp" | "buffer") {
                        ensure!(coded.is_err(), "invalid driver output must fail before use");
                        break;
                    }
                    let coded = coded?;
                    ensure!(
                        coded.len() == 1 && coded[0].pts == frame.pts && coded[0].is_keyframe,
                        "output did not preserve frame/IDR identity"
                    );
                    ensure!(!coded[0].data.is_empty(), "empty output");
                    encoder.set_bitrate(900)?;
                }
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({"case":case,"passed":true,"emulated_runtime":true,"hardware_used":false})
    );
    Ok(())
}
