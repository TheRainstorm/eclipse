//! End-to-end pipeline test: capture -> H.264 -> UDP -> decode -> PSNR.
//!
//! Runs headless (synthetic source) and on loopback, so it is safe in CI.

use uu_core::SelfLoopConfig;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipeline_round_trips_with_acceptable_quality() {
    let cfg = SelfLoopConfig {
        frames: 30,
        width: 320,
        height: 180,
        fps: 60, // pace fast: this is a correctness test, not a benchmark
        bitrate_kbps: 2_000,
        mtu: uu_proto::pkt::DEFAULT_MTU,
        min_psnr_db: 30.0,
    };
    uu_core::selfloop::run(cfg)
        .await
        .expect("self-loop pipeline must complete and pass its quality gate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tiny_mtu_forces_heavy_fragmentation() {
    // A 200-byte MTU leaves very little payload per packet, exercising the
    // reassembler much harder than the default MTU does.
    let cfg = SelfLoopConfig {
        frames: 12,
        width: 160,
        height: 90,
        fps: 60,
        bitrate_kbps: 1_000,
        mtu: 200,
        min_psnr_db: 25.0,
    };
    uu_core::selfloop::run(cfg)
        .await
        .expect("fragmented pipeline must reassemble correctly");
}
