Based on crates.io webrtc-ice 0.11.0 (MIT OR Apache-2.0; original licenses retained).

`UDPMuxDefault::new_with_worker` returns the existing receive task's JoinHandle.
The original `new` discards that handle as before. Receive, buffering, routing,
and close notification behavior are unchanged. The optional native dedicated I/O
experiment awaits this task before retiring its socket driver.

`UDPMuxDefault::get_conn` and address registration hold the close-state mutex
through their map registration. Creation and `close` therefore share the lock
order `closed_watch_tx -> conns/address_map`; an open connection cannot be
inserted after `close` has drained the maps. Per-ufrag close observers retain a
`Weak<UDPMuxDefault>` instead of an `Arc`, so a pending observer cannot form a
mux/connection shutdown retention cycle. Socket routing and packet buffering
remain unchanged. Real concurrent close/create, retained-connection weak-release,
and loopback encode/decode shutdown regressions live in the engine tests.

STUN Binding discovery retransmits the same transaction from 500ms with
exponential backoff (RFC 8489 §6.2.1), up to seven sends within the caller's
unchanged total deadline. A zero deadline retains the existing unbounded receive
contract. Responses must match the server, transaction ID and Binding response
type; unrelated datagrams do not complete or restart the transaction. Owned
loopback tests cover an initially lost request, unrelated responses and expiry.
