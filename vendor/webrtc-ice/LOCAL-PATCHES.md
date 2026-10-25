Based on crates.io webrtc-ice 0.11.0 (MIT OR Apache-2.0; original licenses retained).

`UDPMuxDefault::new_with_worker` returns the existing receive task's JoinHandle.
The original `new` discards that handle as before. Receive, buffering, routing,
and close notification behavior are unchanged. The optional native dedicated I/O
experiment awaits this task before retiring its socket driver.
