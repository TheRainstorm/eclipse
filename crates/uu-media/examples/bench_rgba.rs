use std::time::Instant;
fn main() {
    let (w, h) = (1920u32, 1080u32);
    let mut src = vec![128u8; (w * h * 3 / 2) as usize];
    for (i, b) in src.iter_mut().enumerate() { *b = (i % 251) as u8; }
    // warmup
    let _ = uu_media::convert::i420_to_rgba(&src, w, h);
    let n = 30;
    let t = Instant::now();
    for _ in 0..n { let out = uu_media::convert::i420_to_rgba(&src, w, h); std::hint::black_box(out); }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("i420_to_rgba 1080p: {ms:.2} ms/frame -> {:.0} fps", 1000.0/ms);
}
