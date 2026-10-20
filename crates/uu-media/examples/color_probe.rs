//! CPU-only colour metadata regression: no screen, audio or input devices.
#[path = "support/color_cases.rs"]
mod cases;

fn main() -> anyhow::Result<()> {
    use uu_media::{color, H264Encoder, PixelFormat, RawFrame, Timestamp, VideoEncoder};
    for &(name, data, expected) in cases::CASES {
        anyhow::ensure!(
            format!("{:?}", color::from_h264(data).unwrap()) == expected,
            "H.264 colour metadata failed for {name}"
        );
    }
    let frame = RawFrame {
        width: 64,
        height: 64,
        stride: 64,
        format: PixelFormat::I420,
        data: vec![128; 64 * 64 * 3 / 2].into(),
        pts: Timestamp(0),
        gpu: false,
        dma_buf: None,
    };
    let encoded = H264Encoder::new(1000)?.encode(&frame)?;
    anyhow::ensure!(
        !encoded.is_empty(),
        "software encoder did not emit its first frame"
    );
    anyhow::ensure!(
        encoded
            .iter()
            .any(|frame| color::from_h264(&frame.data) == Some(color::YuvColor::Full601)),
        "real OpenH264 stream colour metadata was not recovered"
    );
    println!(
        "{}",
        serde_json::json!({"color":"passed","sps_cases":cases::CASES.len(),"real_encoded_frame":true,"screen_used":false,"input_used":false})
    );
    Ok(())
}
