use super::*;
use codec::{Decoder, Encoder as CodecEncoder, Parity, GROUP};
use std::time::Duration;
use webrtc::rtp::header::Header;
fn packets(base: u16, timestamp: u32) -> Vec<Packet> {
    (0..GROUP)
        .map(|i| Packet {
            header: Header {
                version: 2,
                ssrc: 1234,
                payload_type: 102,
                sequence_number: base.wrapping_add(i as u16),
                timestamp,
                marker: i + 1 == GROUP,
                ..Default::default()
            },
            payload: vec![(i * 19 + 5) as u8; if i == GROUP - 1 { 123 } else { 1000 }].into(),
        })
        .collect()
}
fn protect(packets: &[Packet]) -> Bytes {
    let mut encoder = CodecEncoder::default();
    let mut parity = None;
    for packet in packets {
        parity = encoder.push(packet).or(parity);
    }
    parity.expect("full group earns protection credit")
}
#[test]
fn each_single_erasure_is_recovered_with_parity_first_last_reordering_and_sequence_wrap() {
    for base in [0, 65532] {
        for missing in 0..GROUP {
            for early in [false, true] {
                let packets = packets(base, 9000);
                let parity = protect(&packets);
                let mut decoder = Decoder::default();
                decoder.source(1234, 102);
                let now = Instant::now();
                let mut recovered = Vec::new();
                if early {
                    recovered.extend(decoder.parity(Parity::parse(&parity).unwrap(), now));
                }
                for i in (0..GROUP).rev().filter(|i| *i != missing) {
                    recovered.extend(decoder.packet(&packets[i], now));
                }
                if !early {
                    recovered.extend(decoder.parity(Parity::parse(&parity).unwrap(), now));
                }
                assert_eq!(recovered, vec![packets[missing].clone()]);
                assert!(decoder
                    .parity(Parity::parse(&parity).unwrap(), now)
                    .is_empty());
                assert!(decoder.packet(&packets[missing], now).is_empty());
            }
        }
    }
}
#[test]
fn two_erasures_wait_for_a_source_packet_and_never_invent_data() {
    let packets = packets(42, 123);
    let parity = protect(&packets);
    let mut decoder = Decoder::default();
    decoder.source(1234, 102);
    let now = Instant::now();
    for packet in &packets[2..] {
        assert!(decoder.packet(packet, now).is_empty());
    }
    assert!(decoder
        .parity(Parity::parse(&parity).unwrap(), now)
        .is_empty());
    assert_eq!(decoder.packet(&packets[0], now), vec![packets[1].clone()]);
}
#[test]
fn malformed_scope_conflicts_expiry_and_replayed_groups_do_not_create_packets() {
    let packets = packets(42, 123);
    let parity = protect(&packets);
    let now = Instant::now();
    for n in 0..parity.len() {
        assert!(Parity::parse(&parity[..n]).is_none());
    }
    for (offset, value) in [
        (0, 0),
        (10, 0),
        (10, 1),
        (10, 9),
        (11, 1),
        (12, 255),
        (13, 255),
    ] {
        let mut bad = parity.to_vec();
        bad[offset] = value;
        assert!(Parity::parse(&bad).is_none());
    }
    let mut decoder = Decoder::default();
    decoder.source(1234, 102);
    for original in &packets[1..] {
        let mut wrong = original.clone();
        wrong.header.ssrc = 5678;
        decoder.packet(&wrong, now);
    }
    assert!(decoder
        .parity(Parity::parse(&parity).unwrap(), now)
        .is_empty());
    for packet in &packets[1..] {
        assert!(decoder
            .packet(packet, now + Duration::from_millis(250))
            .is_empty());
    }
    let mut decoder = Decoder::default();
    decoder.source(1234, 102);
    for packet in &packets[1..] {
        let mut wrong = packet.clone();
        wrong.header.timestamp += 1;
        decoder.packet(&wrong, now);
    }
    assert!(decoder
        .parity(Parity::parse(&parity).unwrap(), now)
        .is_empty());
    let mut decoder = Decoder::default();
    decoder.source(1234, 102);
    for packet in &packets[1..] {
        let mut wrong = packet.clone();
        wrong.header.marker = false;
        decoder.packet(&wrong, now);
    }
    assert!(decoder
        .parity(Parity::parse(&parity).unwrap(), now)
        .is_empty());
}
#[test]
fn parity_budget_is_bounded_and_never_holds_single_packet_frames() {
    let mut encoder = CodecEncoder::default();
    let mut source = 0;
    let mut protection = 0;
    let mut count = 0;
    for frame in 0..200u32 {
        let packets = packets((frame * GROUP as u32) as u16, frame * 3000);
        for packet in packets {
            source += packet.payload.len();
            if let Some(parity) = encoder.push(&packet) {
                assert!(parity.len() <= codec::MAX_WIRE);
                protection += parity.len() + 128;
                count += 1;
            }
        }
    }
    assert!(count >= 190);
    assert!(protection * 5 <= source);
    let mut packet = packets(1, 1).remove(0);
    packet.header.marker = true;
    assert!(encoder.push(&packet).is_none());
}
#[test]
fn negotiation_is_exact_optional_and_duplicate_attributes_do_not_enable_fec() {
    assert!(!supported("v=0\r\n"));
    assert!(supported(&format!("v=0\r\n{ATTRIBUTE}\r\n")));
    assert!(!supported(&format!("{ATTRIBUTE}\r\n{ATTRIBUTE}\r\n")));
    assert!(!supported(
        "a=x-eclipse-video-fec:future-unknown-version\r\n"
    ));
}
