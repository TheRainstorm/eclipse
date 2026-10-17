# 归档：已废弃的 TURN / PCK 输入通道

这些文件实现了早期关于「键盘走 TURN-over-TLS 中继 / PCK 帧」的推断。该方向
后来被证明是错的：官方输入实际走 WebRTC `CONTROL_DATA_CHANNEL` 数据通道上的
**裸 JSON**（见 `crates/uu-core/src/control_wire.rs` 与 `docs/blog-uu-remote-input.md`）。

为保留逆向证据、避免丢失，这些文件从构建中移除并归档到这里，不再编译：

- `uu-core-turn.rs`   —— TURN-over-TLS 客户端（Allocate / CreatePermission / Send/Data Indication）
- `uu-core-pck.rs`    —— PCK 帧编解码 + ICE-TCP/TLS 拨号
- `uu-core-stun.rs`   —— STUN Binding（ICE-TCP 用）
- `uu-core-sdp.rs`    —— SDP 候选解析（relay/srflx/passive TCP）
- `uu-proto-pck.rs`   —— PCK 帧编码

背景：`HANDOVER-RUST-TURN-2026-10-17.md`、`docs/protocol-notes.md`。
