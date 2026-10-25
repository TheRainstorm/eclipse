# Windows capture/encode overlap experiment

`UU_CAPTURE_ENCODE_OVERLAP` accepts exactly `0` (the default) or `1`. This is an
unaccepted experiment, not a production latency recommendation. It requires
unscaled, unrotated, same-adapter DXGI GPU capture; unsupported paths fail
explicitly. Encoding and AMF submission/output collection keep their existing
synchronous driver owner, output cadence and bounded encoded-AU queue.

The capture owner uses a bounded DXGI source-update wait and publishes one
replaceable pending raw image. A consumer retains its own immutable frame lease;
only pending, unencoded images may be replaced. The candidate capture pool has
at most three snapshots: encoding, pending and producer construction. The default
path retains its two-snapshot pool. Reuse requires exclusive ownership and the
existing protected immediate-context GPU ordering. No new unsafe Send/Sync
implementation or CPU image copy is introduced into the media path.

States are running, paused, retiring and stopped. Pause invalidates old leases
and clears pending images. Retired desktop AUs cannot leave the encoded queue;
resume starts a new reference chain. An AU accepted for transmission before
retirement finishes under the existing packet/reference contract; bytes already
sent cannot be undone. Retirement rejects subsequent AU acceptance. Configuration has an acknowledgement and
retires pending work before the encoder generation changes. No old pending
slot is retagged: the source owner confirms the current desktop again before
acknowledgement. A static current image is an explicit duplicate retaining its
original observation times, not a new source frame. Stop wakes capture
and waits at most two seconds for its owner; an unjoined owner is an error and is
never logged as clean. Driver cancellation/teardown remains separately bounded
by the existing encoder interface.

Capture start/ready belong to the source owner, including images waiting in the
slot. Repeated output is explicit, retains observation times and advances only
output PTS. Frame trace v2 adds capture-ready-to-encode slot wait and
source-present-to-last-RTP-API-return on the source host's QPC clock. Only known,
new source frames with matching clock frequency enter the latter distribution.
The bounded 512-frame rolling window exports its QPC tuples and integer mean so
analysis can replay it. Telemetry remains at one report per second, capped at
64KiB; this is not a GPU completion, scanout or photon measurement. Older peers
without the v2 diagnostic capability can stream without this diagnostic channel.

`capture_overlap_probe` exercises real DXGI capture through this same handoff,
holds frames while newer images are captured, verifies immutable pixels and
a 16-bit source marker with its complement across encoded PTS, and checks
AV1 AMF/MF GPU PTS/pixel identity, and checks lease release across recreation.
Its readback and deliberate blocking are correctness checks, excluded from
performance measurements. Unit tests and a short run do not establish desktop
transition, device-loss, HDR, interaction or long-term production acceptance.

The external worker records product thread counts from read-only Toolhelp
snapshots at most once per second; query/close errors are retained as unknown,
not converted to zero. This is separate from capture-owner lifecycle counters.
