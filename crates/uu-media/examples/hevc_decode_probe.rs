//! Decode the bundled generated gray video; no desktop, network or input.
use anyhow::ensure;
use uu_media::{EncodedFrame, Timestamp};

fn main() -> anyhow::Result<()> {
    let leases = std::env::args().skip(1).any(|arg| arg == "--leases");
    uu_media::dma_buf::set_rendering(leases);
    uu_media::hevc::probe_decoder()?;
    let codec = uu_proto::media::VideoCodec::Hevc;
    let mut decoder = uu_media::decoder_for_codec(codec)?;
    ensure!(decoder.codec() == codec, "wrong decoder codec");
    let name = decoder.backend_name().to_owned();
    let data = uu_media::hevc::PROBE;
    let mut starts = Vec::new();
    for i in 0..data.len().saturating_sub(4) {
        if data[i..].starts_with(&[0, 0, 0, 1, 70]) {
            starts.push(i);
        }
    }
    starts.push(data.len());
    let mut submitted = std::collections::HashSet::new();
    let mut retained = std::collections::VecDeque::new();
    let (mut decoded, mut gpu) = (0, 0);
    let mut timing = Vec::new();
    for iteration in 0..30 {
        for (index, span) in starts.windows(2).enumerate() {
            let pts = Timestamp(1_000_000 + (iteration * 4 + index as u64) * 16_667);
            submitted.insert(pts);
            let bytes = &data[span[0]..span[1]];
            let now = std::time::Instant::now();
            if let Some(frame) = decoder.decode(&EncodedFrame {
                data: bytes.to_vec().into(),
                pts,
                is_keyframe: uu_media::hevc::is_keyframe(bytes),
            })? {
                ensure!(
                    (frame.width, frame.height) == (128, 128) && frame.is_consistent(),
                    "invalid decoded frame"
                );
                ensure!(submitted.contains(&frame.pts), "transport timestamp lost");
                if leases {
                    ensure!(
                        frame.gpu && frame.dma_buf.is_some(),
                        "GPU export unavailable"
                    );
                    gpu += 1;
                } else {
                    ensure!(
                        !frame.gpu && frame.data.len() >= 128 * 128 * 3 / 2,
                        "CPU pixels unavailable"
                    );
                }
                decoded += 1;
                retained.push_back(frame);
                if retained.len() > 3 {
                    retained.pop_front();
                }
            }
            timing.push(now.elapsed().as_secs_f64() * 1000.);
        }
    }
    ensure!(
        decoded >= 118,
        "too few pictures from complete access units: {decoded}"
    );
    timing.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::json!({"codec":"HEVC","backend":name,"decoded":decoded,"gpu_frames":gpu,
        "transport_pts_preserved":true,"surface_leases_checked":leases,"decode_ms_p50":timing[timing.len()/2],"decode_ms_p95":timing[timing.len()*95/100]})
    );
    Ok(())
}
