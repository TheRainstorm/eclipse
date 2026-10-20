# Local SRTP rollover patch

Base: crates.io `webrtc-srtp` 0.13.0, checksum `fbec5da43a62c228d321d93fb12cc9b4d9c03c9b736b0c215be89d8bd0774cfe`. Upstream MIT license is retained.

The original state used a 100-packet boundary heuristic and replaced its last sequence number on every encryption/decryption. A late retransmission or loss covering the first 100 packets after sequence rollover can desynchronize subsequent authenticated packets.

`context/mod.rs` now estimates the 48-bit packet index around the highest accepted index with a half-sequence-space comparison, and advances state only for newer indices. Index exhaustion returns an error instead of wrapping to a previously used range. `context/srtp.rs` propagates that error. Cipher, key derivation, authentication and replay detection implementations are unchanged.

References: [RFC 3711 §3.3.1](https://www.rfc-editor.org/rfc/rfc3711.html#section-3.3.1) and the highest-index approach in [Pion SRTP context](https://github.com/pion/srtp/blob/main/context.go). The Rust change preserves this crate's existing state fields and API outside its private state implementation.

Validation: the upstream library's 25 tests pass, including cipher vectors, session encryption and replay checks. Project regressions cover both supported profiles, retransmission ciphertext equality, more than 100 lost packets across rollover, forged authentication tags and duplicate rejection. The standalone synthetic `srtp_retransmit_probe` reproduces the pre-patch failure and passes with this patch.

An empty workspace table allows standalone upstream tests despite an enclosing application workspace. The standalone test lockfile and build directory are ignored; application builds use the root Cargo.lock.
