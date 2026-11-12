Based on webrtc 0.11.0 (MIT OR Apache-2.0; original licenses retained).

`RTCPeerConnection::gather_ice_candidates` exposes the existing candidate
gathering operation without changing signaling state or creating media tracks.
`set_local_description` uses the same operation, so complete-SDP signaling can
overlap discovery with application preparation and later reuse the candidates
and credentials. Calls follow the existing serialized description lifecycle;
no new background owner or transport is introduced. Connection close still owns
all gathered resources. Eclipse's owned startup regression checks discovery
before SDP, late data-channel creation, complete candidates and shutdown.
