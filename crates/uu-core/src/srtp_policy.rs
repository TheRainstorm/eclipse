//! Keep authenticated repair within the bounded RTP reorder horizon.
use webrtc::api::setting_engine::SettingEngine;

// The application already retains up to 2048 RTP packets. The dependency's
// default replay window (64) otherwise rejects an unseen repair long before
// the reorder queue can consider it. This remains a fixed bitmap per SSRC.
pub(crate) const REPLAY_WINDOW_PACKETS: usize = crate::video_receiver::MAX_PACKETS;
pub(crate) fn configure(settings: &mut SettingEngine) {
    settings.set_srtp_replay_protection_window(REPLAY_WINDOW_PACKETS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use webrtc::{
        rtp::{header::Header, packet::Packet},
        srtp::{
            context::Context, option::srtp_replay_protection, protection_profile::ProtectionProfile,
        },
        util::marshal::Marshal,
    };
    #[test]
    fn unseen_repairs_survive_reordering_but_forgery_replay_and_expired_packets_do_not() {
        for profile in [
            ProtectionProfile::Aes128CmHmacSha1_80,
            ProtectionProfile::AeadAes128Gcm,
        ] {
            for window in [64, REPLAY_WINDOW_PACKETS] {
                let key = [0x17; 16];
                let salt = vec![
                    0x2a;
                    if matches!(profile, ProtectionProfile::Aes128CmHmacSha1_80) {
                        14
                    } else {
                        12
                    }
                ];
                let mut sender = Context::new(&key, &salt, profile, None, None).unwrap();
                let mut receiver = Context::new(
                    &key,
                    &salt,
                    profile,
                    Some(srtp_replay_protection(window)),
                    None,
                )
                .unwrap();
                let mut withheld = Vec::new();
                for seq in (65500..=65535).chain(0..=3000) {
                    let plain = Packet {
                        header: Header {
                            version: 2,
                            payload_type: 102,
                            ssrc: 42,
                            sequence_number: seq,
                            ..Default::default()
                        },
                        payload: bytes::Bytes::from_static(b"generated repair regression"),
                        ..Default::default()
                    }
                    .marshal()
                    .unwrap();
                    let wire = sender.encrypt_rtp(&plain).unwrap();
                    if seq == 65530 || seq == 65531 {
                        withheld.push((plain, wire));
                        continue;
                    }
                    assert_eq!(receiver.decrypt_rtp(&wire).unwrap(), plain);
                    if seq == 400 {
                        let (plain, wire) = &withheld[0];
                        let mut forged = wire.to_vec();
                        *forged.last_mut().unwrap() ^= 1;
                        assert!(
                            receiver.decrypt_rtp(&forged).is_err(),
                            "forgery must not consume an unseen index"
                        );
                        let repair = receiver.decrypt_rtp(wire);
                        if window == 64 {
                            assert!(
                                repair.is_err(),
                                "baseline must reproduce premature rejection"
                            );
                        } else {
                            assert_eq!(
                                repair.unwrap(),
                                *plain,
                                "authenticated, previously unseen repair must survive"
                            );
                        }
                        assert!(
                            receiver.decrypt_rtp(wire).is_err(),
                            "duplicates must still be rejected"
                        );
                    }
                }
                assert!(
                    receiver.decrypt_rtp(&withheld[1].1).is_err(),
                    "old unseen packets beyond the window must expire"
                );
            }
        }
    }
}
