//! Generated AV1 I/P temporal units, no desktop, input, network or sound.
use anyhow::ensure;
use uu_media::{EncodedFrame, Timestamp};
fn main() -> anyhow::Result<()> {
    let leases = std::env::args().skip(1).any(|arg| arg == "--leases");
    uu_media::dma_buf::set_rendering(leases);
    uu_media::av1::probe_decoder()?;
    let mut decoder = uu_media::decoder_for_codec(uu_proto::media::VideoCodec::Av1)?;
    decoder.prepare(128, 128)?;
    let name = decoder.backend_name().to_owned();
    let mut retained = std::collections::VecDeque::new();
    let mut times = Vec::new();
    let mut count = 0;
    for (index, data) in uu_media::av1::PROBE_UNITS
        .iter()
        .cycle()
        .take(120)
        .enumerate()
    {
        let pts = Timestamp(1_000_000 + index as u64 * 16667);
        let start = std::time::Instant::now();
        if let Some(frame) = decoder.decode(&EncodedFrame {
            data: bytes::Bytes::from_static(data),
            pts,
            is_keyframe: uu_media::av1::is_keyframe(data),
        })? {
            ensure!(
                (frame.width, frame.height) == (128, 128) && frame.is_consistent(),
                "invalid AV1 pixels"
            );
            ensure!(
                frame.pts == pts,
                "AV1 temporal unit lost its presentation timestamp"
            );
            if leases {
                ensure!(
                    frame.gpu && frame.dma_buf.is_some(),
                    "GPU export unavailable"
                );
            }
            retained.push_back(frame);
            if retained.len() > 3 {
                retained.pop_front();
            }
            count += 1;
        }
        times.push(start.elapsed().as_secs_f64() * 1000.);
    }
    ensure!(count >= 118, "too few AV1 pictures: {count}");
    times.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"codec":"AV1","backend":name,"decoded":count,"submitted":120,"gpu_leases":leases,"decode_ms_p50":times[60],"decode_ms_p95":times[114],"desktop_used":false})
    );
    Ok(())
}
