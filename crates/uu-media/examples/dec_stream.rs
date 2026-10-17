//! Decode an Annex-B H.264 file with OpenH264 to isolate decoder issues.
//!
//! Usage: cargo run -p uu-media --example dec_stream -- /tmp/stream.h264 [noflush]

use openh264::decoder::{Decoder, DecoderConfig, Flush};
use openh264::OpenH264API;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dec_stream <file.h264> [noflush]");
    let mode = std::env::args().nth(2).unwrap_or_default();
    let data = std::fs::read(&path).expect("read file");

    let mut decoder = if mode.contains("noflush") || mode == "debug" || mode.starts_with("ec") {
        let mut cfg = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        if mode == "debug" {
            cfg = cfg.debug(true);
        }
        Decoder::with_api_config(OpenH264API::from_source(), cfg).unwrap()
    } else {
        Decoder::new().unwrap()
    };

    if let Some(ec) = mode.strip_prefix("ec") {
        let ecv: i32 = ec.parse().unwrap_or(1);
        let mut v = ecv;
        unsafe {
            let _ = decoder
                .raw_api()
                .set_option(8, &mut v as *mut i32 as *mut std::os::raw::c_void);
        }
        eprintln!("set ERROR_CON_IDC={ecv}");
    }

    // Split into access units at each AUD (NAL type 9) or SPS (7) start code,
    // which is how RTP depacketization hands us samples.
    let units = if mode.contains("nal") {
        split_nals(&data)
    } else {
        split_access_units(&data)
    };
    let mut ok = 0u32;
    let mut none = 0u32;
    let mut errs = 0u32;
    for (i, unit) in units.iter().enumerate() {
        match decoder.decode(unit) {
            Ok(Some(_)) => ok += 1,
            Ok(None) => none += 1,
            Err(e) => {
                errs += 1;
                if errs <= 10 {
                    eprintln!("unit {i} len={} err: {e}", unit.len());
                }
            }
        }
    }
    println!("mode={mode:?} units={} ok={ok} none={none} err={errs}", units.len());
}

fn split_access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        let sc = if data[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if data[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        let h = i + sc;
        if h < data.len() {
            let nal = data[h] & 0x1f;
            if nal == 9 {
                starts.push(i);
            }
        }
        i = h + 1;
    }
    starts.push(data.len());
    let mut units = Vec::new();
    for w in starts.windows(2) {
        if w[1] > w[0] {
            units.push(data[w[0]..w[1]].to_vec());
        }
    }
    units
}

fn split_nals(data: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i..].starts_with(&[0, 0, 0, 1]) || data[i..].starts_with(&[0, 0, 1]) {
            starts.push(i);
            i += 1;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    let mut units = Vec::new();
    for w in starts.windows(2) {
        if w[1] > w[0] {
            units.push(data[w[0]..w[1]].to_vec());
        }
    }
    units
}
