//! Offline benchmark for the CPU upload path; no account or remote connection.
//! cargo run --release -p uu-media --example convert_bench -- [frames]
use std::{hint::black_box, time::Instant};
fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .map(|n| n.parse().expect("frames"))
        .unwrap_or(60)
        .clamp(1, 1000);
    for (w, h, stride) in [(1920, 1080, 2048), (3840, 2160, 4096)] {
        let mut seed = 1u32;
        let src: Vec<u8> = (0..stride * h * 3 / 2)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect();
        for layout in ["NV12", "I420"] {
            let convert = || {
                if layout == "NV12" {
                    uu_media::convert::nv12_to_rgba(black_box(&src), w, h, stride)
                } else {
                    uu_media::convert::i420_to_rgba_strided(black_box(&src), w, h, stride)
                }
            };
            black_box(convert());
            let started = Instant::now();
            let mut fingerprint = 0u64;
            for _ in 0..count {
                let pixels = convert();
                fingerprint = black_box(
                    pixels
                        .iter()
                        .step_by(4093)
                        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u64)),
                );
                black_box(pixels);
            }
            println!("{layout} {w}x{h} stride={stride} frames={count} ms_per_frame={:.3} fingerprint={fingerprint:016x}",started.elapsed().as_secs_f64()*1000.0/count as f64);
        }
    }
}
