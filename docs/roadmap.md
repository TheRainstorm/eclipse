# 路线图 —— 原生 Rust 重写（Windows + Linux）

## 策略

1. **媒体面不自研**：官方用 libwebrtc（见 `protocol-notes.md`），我们用 Rust 的
   WebRTC 实现承载 ICE/DTLS-SRTP/Opus/H.264-HEVC。
2. **先自证，再互通**：先让我们自己的 host 与 controller 两端跑通（不碰网易服务器），
   把采集/编码/传输/解码/输入/渲染全链路验证完；之后再接官方信令。
3. **平台差异全部收敛在两个 trait 后面**：`uu_capture::Capture` 与
   `uu_input::InputInjector`。

## 里程碑

| # | 目标 | 验收方式 | 状态 |
|---|---|---|---|
| M0 | 工程骨架、协议类型、状态机 | `cargo test --workspace` 全绿 | ✅ 完成 |
| M1 | 发布源/版本探测（真实可用） | `uu release-check` 命中官方 feed | ✅ 完成 |
| M2a | **自有环路（UDP）**：采集→H.264→分片→UDP→重组→解码+质量门禁 | `uu self-loop` 精确对账 + PSNR≥30dB | ✅ 完成 |
| M2b | 传输换成真 WebRTC（ICE/DTLS-SRTP），并加 NACK/RTX | 丢包下仍能恢复，长跑无卡顿 | 进行中 |
| M2c | controller 端渲染出画面（真实窗口，而非仅统计） | 人眼可见远端画面 | 待做 |
| M3 | 原生输入注入闭环 | controller 的鼠标键盘驱动 host 桌面（uinput/XTest 已验证可用性） | 待做 |
| M4 | Linux 主机侧采集 | PipeWire(XDG portal) + X11 SHM 双后端 | 待做 |
| M5 | Windows 端 | WGC/DXGI 采集 + SendInput（`windows-sys`，需在 Windows 上编译验证） | 待做 |
| M6 | 接入官方服务 | 复现信令 + 鉴权，与官方客户端互通 | 🟢 鉴权+注册+扫码登录+设备列表已跑通；🟡 room/join 已通，TLS 内协议待解 |

## 已落地的代码

```
crates/uu-proto   帧格式(含测试)、控制消息、媒体协商参数、ID
crates/uu-media   RawFrame/EncodedFrame、PixelFormat、编解码 trait、NullEncoder
crates/uu-capture Capture trait + 后端探测(Linux: pipewire/x11-shm, Windows: wgc/dxgi)
crates/uu-input   InputInjector trait；Linux uinput 实现（真实 ioctl）；Windows SendInput 实现
crates/uu-core    Config(XDG)、SignalClient(签名端点)、Session 状态机(含转移表测试)
crates/uu-cli     uu probe / release-check / devices / control / host / self-loop
```

### M2a 实测数据（ares，release 构建）

| 场景 | 帧数 | 包数 | 编码字节 | 解码 | PSNR |
|---|---|---|---|---|---|
| 1280×720 | 150 | 1361（**全部对账**） | 1.47 MB | 149/150* | 56.4 dB |
| 640×360 | 120 | 467 | 449 KB | 120/120 | 56.0 dB |

\* 差的 1 帧是 **OpenH264 的一帧编码延迟**：会话结束前必须 flush，否则最后一帧丢。
harness 会显式报出这个差值，而不是把它算成丢包。

### M2a 已实现的能力

- **真编解码**：`openh264` 0.9.9 从源码构建（x86_64 需要 `nasm`），软编软解，无需 GPU。
- **色彩转换**：NV12→I420、BGRA→I420（BT.601），均正确处理 stride 对齐。
- **合成采集源**：无显示器也能跑全链路（CI 友好），图案含运动与色度变化。
- **分片/重组**：20 字节包头，按 MTU 切分，容忍乱序与重复，丢弃过期帧。
- **质量门禁**：PSNR + 包数精确对账 + `incomplete==0` + `decoded==encoded`。

## 已知风险

* **鉴权**（M6 前置）：`libnrdsign.so` 签名 + 设备指纹 + WebView2 Web SSO。
  在还原之前，native 客户端**无法登录官方账号**。这是最大不确定性。
* **Windows 端无法在 Linux 上编译验证**：需要一台 Windows 或 mingw 工具链做 CI 验证；
  `uu-input/src/windows.rs` 目前未编译验证。
* **被控端的采集**：官方用 IDD 造虚拟显示器；Linux 侧需用 compositor 输出
  （真实显示器 / Xorg dummy / Xvfb）。无头场景要显式指定。
