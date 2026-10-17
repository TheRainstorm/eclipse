# uu_remote — UU 远程的原生 Rust 重写（Windows + Linux）

目标：不依赖 Wine，用 Rust 原生实现网易 UU 远程（`uuyc.163.com`）的**被控端**与
**主控端**，同时支持 Windows 与 Linux 两个平台。

> 说明：网易官方只发布 Windows / macOS / Android / iOS / AndroidTV 客户端，**没有
> Linux 版**。本仓库是对其实现的独立重写，不是网易产品，与网易无关。

## 现状

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | 工程骨架、协议类型与帧格式、会话状态机 | ✅ |
| M1 | 官方发布源/版本探测 | ✅ |
| M2a | 自有环路（UDP）：采集→H.264→分片→UDP→解码→PSNR | ✅ |
| M2b | 官方 WebRTC 媒体面（ICE/DTLS-SRTP、H264、多显示器） | ✅ 已收到 1080p 画面 |
| M3 | 输入：鼠标/拖拽经 PCK over data channel | ✅ 实测生效 |
| M3b | 键盘输入 | 🟡 Rust TURN/PCK 已接通；远端 VK 注入仍待验收 |
| M4 | 官方 API：签名、扫码登录、设备列表、room/join | ✅ |
| M5 | 官方信令：Socket.IO (`X-NRD-AUTH`) + SDP/ICE | ✅ |
| M6 | GUI：设备列表、扫码登录、远程画面、全屏/断开 | ✅ |
| M7 | Windows x64 交叉构建 | ✅ 已在 KVM-WIN11 启动验证 |

> 现在已经能做到：连接官方被控端、连续看画面、用鼠标和键盘操作。剩余的是
> 音频/剪贴板/文件等外围功能，以及原生采集后端（目前是占位）。

## 最重要的逆向结论

**官方媒体面就是标准 libwebrtc。** `libstreamer.so` 内嵌完整 WebRTC，含
`webrtc/pc/ice_server_parsing.cc`、`h264_sprop_parameter_sets.cc`、`ssl_fingerprint.cc`
等编译期路径，SDP 标记（`a=group:BUNDLE`、`ice-ufrag`、`candidate:`、`fingerprint`）、
Opus 编码器导出、`org.webrtc` JNI 一应俱全。

因此：**媒体面用 Rust 的 WebRTC 实现即可，工作量集中在信令与鉴权**。
完整证据链见 [`docs/protocol-notes.md`](docs/protocol-notes.md)。

## 快速开始

```bash
cargo test --workspace
cargo run --bin uu -- devices                 # 列出账号下的设备
cargo run --bin uu -- control <device-id>     # 信令 + WebRTC + 视频（CLI 调试）
cargo run -p uu-gui                           # 图形客户端
```

Windows 版（交叉编译产物）：

```
dist/uu-gui-win64/
  uu-gui.exe
  libstdc++-6.dll
  libgcc_s_seh-1.dll
  README.txt
```

首次启动会显示二维码，用手机扫码登录；左侧选择设备后点“连接”。

## 仓库结构

```
crates/uu-proto    帧格式、控制消息、媒体协商参数、ID
crates/uu-media    RawFrame/EncodedFrame、像素格式、编解码 trait
crates/uu-capture  采集抽象（Linux: PipeWire/X11，Windows: WGC/DXGI）
crates/uu-input    输入注入抽象（Linux: uinput，Windows: SendInput）
crates/uu-core     配置、信令客户端、会话状态机
crates/uu-cli      uu 可执行文件（调试/自动化）
crates/uu-gui      uu-gui 图形客户端（eframe/egui）
tools/win          Frida 抓包/挂钩脚本
tools/linux_controller  已验证的 Python/aiortc 参考实现
dist/              Windows 发布包
docs/              协议逆向笔记与路线图
notes/             Linux/Wine 兼容层的可行性调研（前期结论，见下）
artifacts/         采样样本与校验和（不随仓库分发官方二进制）
```

## 已知限制

- **键盘/鼠标输入已打通**：输入走 `CONTROL_DATA_CHANNEL` 数据通道上的**裸
  JSON**（先做 SimpleAction echo 握手）。已在 KVM-WIN11 上用低层键盘钩子
  验证 `VK` 注入、并用点击开始菜单验证鼠标。
- 被控端 TCP host candidate 在 STUN 后说的是 `0x04` 自定义协议，不是普通
  TLS；不要直接向该端口发 ClientHello。
- 音频（Opus）尚未接入。
- 只实现了主控端（controller）；被控端（host）的桌面采集/编码还没接。
- 视频只解 H264；H265 需要额外的解码器。
- 鼠标目前走 WebRTC data channel；键盘硬走 TURN/PCK 中继通道，不能复用
  data channel（已用被控端低层键盘钩子确认）。

## 与前期调研的关系

`notes/` 记录了动手前的可行性调研：官方无 Linux 版、安装包载荷加密、以及
把官方 Windows 客户端放进 Wine 的几条既有路线（`uur`、`uu-remote-for-linux` 等）。
该路线最终卡在客户端 UI 主动退出上，因此转向本仓库的**原生重写**。
这些笔记仍有价值：它们给出了客户端组件清单与内核驱动分割线（IDD/ViGEm/uuycinput）。
