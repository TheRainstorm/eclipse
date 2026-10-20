# Local SRTP rollover patch

Base: crates.io `webrtc-srtp` 0.13.0, checksum `fbec5da43a62c228d321d93fb12cc9b4d9c03c9b736b0c215be89d8bd0774cfe`. Upstream MIT license is retained.

The original state used a 100-packet boundary heuristic and replaced its last sequence number on every encryption/decryption. A late retransmission or loss covering the first 100 packets after sequence rollover can desynchronize subsequent authenticated packets.

`context/mod.rs` now estimates the 48-bit packet index around the highest accepted index with a half-sequence-space comparison, and advances state only for newer indices. Index exhaustion returns an error instead of wrapping to a previously used range. `context/srtp.rs` propagates that error. Cipher, key derivation and authentication are unchanged. Replay recording receives the separate correction below.

References: [RFC 3711 §3.3.1](https://www.rfc-editor.org/rfc/rfc3711.html#section-3.3.1) and the highest-index approach in [Pion SRTP context](https://github.com/pion/srtp/blob/main/context.go). The Rust change preserves this crate's existing state fields and API outside its private state implementation.

Validation: the upstream library's 25 tests pass, including cipher vectors, session encryption and replay checks. Project regressions cover both supported profiles, retransmission ciphertext equality, more than 100 lost packets across rollover, forged authentication tags and duplicate rejection. The standalone synthetic `srtp_retransmit_probe` reproduces the pre-patch failure and passes with this patch.

An empty workspace table allows standalone upstream tests despite an enclosing application workspace. The standalone test lockfile and build directory are ignored; application builds use the root Cargo.lock.

## First late arrival across rollover

The dependency's wrapped replay detector checked the normalized age, but marked acceptance using the raw sequence subtraction. A previously unseen packet from the preceding cycle could therefore authenticate without setting its replay bit. `replay_detector.rs` supplies the SRTP option with a bounded ring bitmap: it uses the same normalized age for checking and marking, separates tentative checks from authenticated acceptance, and clears aged-out slots only when the highest accepted sequence advances. SRTCP selection is unchanged.

The option API and default session window remain unchanged. The application separately chooses a 2048-packet SRTP window to match its existing bounded reorder queue; larger windows are permitted by [RFC 3711 §3.3.2](https://www.rfc-editor.org/rfc/rfc3711.html#section-3.3.2). Authentication, duplicate rejection and expiry remain enforced. This is not video FEC.

Tests include both cipher profiles with a first late previous-cycle packet in the default 64-packet window, forged repairs, immediate replay rejection, non-power-of-two bitmap sizes, large jumps, multiple full sequence cycles and failed authentication checks that never advance the window. Original cipher and session vectors remain part of the standalone suite.

Standalone verification now passes 31 tests (the original 25 plus six local regressions), including a 250,000-event deterministic comparison with a full-index reference model.
