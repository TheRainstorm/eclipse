# Local changes to webrtc 0.11.0

Source: crates.io webrtc 0.11.0, upstream commit `8a074005f5833539c54e991fb69fc2963046dbbb`, subdirectory `webrtc`.
Upstream: https://github.com/webrtc-rs/webrtc
License: MIT OR Apache-2.0; upstream license texts retained.

`src/ice_transport/ice_candidate_pair.rs` adds read-only `local()` and `remote()` accessors. This exposes the actual selected pair returned by ICE without parsing Display/Debug text or guessing from nominated candidate statistics. It changes no ICE behavior, packet format or synchronization.
