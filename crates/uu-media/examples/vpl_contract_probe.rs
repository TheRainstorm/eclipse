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
    if case.starts_with("decode_") && case != "decode_alignment" {
        use uu_media::VideoDecoder;
        let result = uu_media::hw::vpl::VplAv1Decoder::new().and_then(|mut decoder| {
            decoder.prepare(128, 128)?;
            decoder.decode(&uu_media::EncodedFrame {
                data: bytes::Bytes::from_static(uu_media::av1::PROBE),
                pts: Timestamp(0),
                is_keyframe: true,
            })
        });
        ensure!(result.is_err(), "invalid decoder driver output must fail");
        println!(
            "{}",
            serde_json::json!({"case":case,"passed":true,"emulated_runtime":true,"hardware_used":false})
        );
        return Ok(());
    }
    if matches!(
        case.as_str(),
        "software" | "fps" | "partial" | "init_fps" | "codec" | "mode"
    ) {
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
                if case.starts_with("low_power_") {
                    ensure!(encoder.backend_name().contains("general engine"), "unsupported/changed low-power engine must fall back explicitly");
                }
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
    if matches!(case.as_str(), "valid" | "decode_alignment") {
        use uu_media::VideoDecoder;
        let mut decoder = uu_media::hw::vpl::VplAv1Decoder::new()?;
        decoder.prepare(128, 128)?;
        let mut coded = uu_media::EncodedFrame {
            data: bytes::Bytes::from_static(uu_media::av1::PROBE),
            pts: Timestamp(0),
            is_keyframe: true,
        };
        let first = decoder
            .decode(&coded)?
            .ok_or_else(|| anyhow::anyhow!("no contract decode output"))?;
        ensure!(
            first.is_consistent() && !first.gpu && first.data[0] == 126 && first.pts == coded.pts,
            "invalid copied frame"
        );
        coded.pts = Timestamp(16666);
        let next = decoder.decode(&coded)?.unwrap();
        ensure!(
            next.data[0] == 128 && first.data[0] == 126 && next.pts == coded.pts,
            "borrowed surface escaped its worker or timestamp changed"
        );
    }
    println!(
        "{}",
        serde_json::json!({"case":case,"passed":true,"emulated_runtime":true,"hardware_used":false})
    );
    Ok(())
}
