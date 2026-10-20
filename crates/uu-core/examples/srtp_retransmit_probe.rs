//! Synthetic SRTP state diagnostic; no sockets, service keys, screen or input.
use webrtc::{
    rtp::{header::Header, packet::Packet},
    srtp::{
        context::Context, option::srtp_replay_protection, protection_profile::ProtectionProfile,
    },
    util::marshal::Marshal,
};
fn main() -> anyhow::Result<()> {
    let profile = ProtectionProfile::Aes128CmHmacSha1_80;
    let mut sender = Context::new(&[0x17; 16], &[0x2a; 14], profile, None, None)?;
    let mut receiver = Context::new(
        &[0x17; 16],
        &[0x2a; 14],
        profile,
        Some(srtp_replay_protection(64)),
        None,
    )?;
    let packet = |seq| {
        Packet {
            header: Header {
                version: 2,
                sequence_number: seq,
                ssrc: 42,
                payload_type: 102,
                ..Default::default()
            },
            payload: bytes::Bytes::from_static(b"synthetic"),
            ..Default::default()
        }
        .marshal()
        .unwrap()
    };
    for seq in (65500..=65535).chain(0..=200) {
        let plain = packet(seq);
        let wire = sender.encrypt_rtp(&plain)?;
        assert_eq!(receiver.decrypt_rtp(&wire)?, plain);
    }
    for seq in [65530, 40] {
        let wire = sender.encrypt_rtp(&packet(seq))?;
        let _ = receiver.decrypt_rtp(&wire);
    }
    let plain = packet(201);
    let wire = sender.encrypt_rtp(&plain)?;
    let fresh = receiver.decrypt_rtp(&wire);
    println!(
        "{}",
        serde_json::json!({"synthetic_only":true,"fresh_packet_after_late_retransmissions":fresh.as_ref().is_ok_and(|data|data==&plain)})
    );
    anyhow::ensure!(
        fresh.as_ref().is_ok_and(|data| data == &plain),
        "late retransmissions desynchronized fresh SRTP packets"
    );
    Ok(())
}
