# uu-client — 开源远程串流客户端（Windows + Linux）

<img src="assets/branding/eclipse-256.png" width="80" height="80" alt="Eclipse 日蚀图标">

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
| M3b | 键盘输入 | ✅ CONTROL 通道 VK 注入已实测 |
| M4 | 官方 API：签名、扫码登录、设备列表、room/join | ✅ |
| M5 | 官方信令：Socket.IO (`X-NRD-AUTH`) + SDP/ICE | ✅ |
| M6 | GUI：设备列表、扫码登录、远程画面、全屏/断开 | ✅ |
| M7 | Windows x64 交叉构建 | ✅ 已在 Windows 测试主机启动验证 |
| M8 | Windows D3D11VA 硬件解码 | ✅ Intel Arc B390 / AMD RX 7900 XTX 实测；Linux VA-API 已在 Linux 测试客户端 Intel Tiger Lake 实测 |
| M9 | 输入与串流体验 | ✅ Windows 中键按住/拖动、浅色设备主页、RTP 重排/坏帧丢弃/恢复、最新帧呈现 |
| M10 | 自动档与重传恢复 | ✅ DecoderCap/自动画质、NACK/RTX、严格参考链恢复、可隐藏悬浮菜单；短测17–19Mbps |

> 现在已经能做到：连接官方被控端、连续看画面、用鼠标和键盘操作。剩余的是
> 文件等外围功能，以及原生采集后端（目前是占位）。

Linux 原生客户端、统一安装入口和独立 Wayland/音频自动验收见 [Linux 客户端说明](docs/linux-client.md)。

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

Windows 版（Linux 交叉编译与打包）：

```bash
python3 tools/windows/package.py
```

产物是 `artifacts/packages/uu-client-windows-x64-<提交号>.zip`，包含 EXE、运行 DLL、安装器和 SHA256 清单。
环境准备和安装方法见 [Windows 编译与打包说明](docs/windows-build.md)。
在“关于软件”查看版本与项目信息，`version.json` 记录源提交号与文件哈希。旧 `dist/uu-gui-win64` 不再更新。

首次启动点击“扫码登录”，用手机扫描二维码；左侧选择设备后点“连接”。
右上角账号菜单提供扫码登录与退出账号。退出会清除本机登录态并断开本客户端的串流，保留设备注册信息和软件设置；取消或刷新二维码会停止旧登录流程。

Windows 默认优先使用 D3D11VA，失败时回退到 OpenH264；设置 `UU_DECODER=sw` 可强制软解。
HUD 显示实际解码后端。离线验证可运行
`uu.exe probe --h264 sample.h264 --frame first-frame.ppm`，同时检查解码数量、尺寸和首帧图像。
当前硬解仍需 CPU 回读，并不保证比软解更快。

设备主页按参考图改成浅色布局；连接在独立串流窗口打开，设备主窗口保留，可继续连接其他设备。Ctrl+Alt+S 切换统计，
Ctrl+Alt+F / X 切换全屏串流，Ctrl+Alt+Q 退出；界面按钮只负责窗口最大化。串流窗口默认在目标显示器工作区80%内等比适配、居中，跨屏自动重新适配。Windows 路径中键使用实测的 `MK_MBUTTON=16`，支持按住拖动。
接收端支持短时 RTP 重排、NACK/RTX 和参考链恢复；FEC 尚未接入。
默认使用官方自动画质，默认保持远端分辨率；全屏悬浮菜单可用 Ctrl+Alt+H 隐藏/恢复。Ctrl+Alt+M 切换静音，串流顶部和全屏悬浮入口也提供声音快捷按钮。
设置菜单 → 画质整合原画/超清/高清、自定义码率、30/60/90/120/144 FPS 和自定义 5–144 FPS。
菜单按官方参考使用黑点选中与灰色悬停，不显示常驻确认小字；默认保持远端显示器布局；软件设置可选UU虚拟屏独占，重新连接生效。顶部标签页与全屏菜单按真实屏幕列表切换，显示名称和主屏标记。
HUD 区分目标帧率、远端屏幕类型/刷新率和实际 Rx/De/Rd；视频更新不代表输入控制通道已就绪。
协议依据、当前边界和测试方法见 [`docs/frame-rate.md`](docs/frame-rate.md)。
恢复机制、卡顿原因和 Sunshine/Moonlight 对比见 [`docs/streaming-quality.md`](docs/streaming-quality.md)。
视频正常而输入SCTP长期connecting的DTLS接收竞态已复现并修补，用户已确认`f74fc9d`恢复键鼠，补丁说明见 [DTLS 接收修复](vendor/webrtc-dtls/UU-PATCH.md)。
三个本地开源项目的功能、源码入口、接入边界与实施顺序见
[`docs/reference-index.md`](docs/reference-index.md)；包含固定提交版本和快速迭代时的更新方法。

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
  JSON**（先做 SimpleAction echo 握手）。已在 Windows 测试主机上用低层键盘钩子
  验证 `VK` 注入、并用点击开始菜单验证鼠标。
- 被控端 TCP host candidate 在 STUN 后说的是 `0x04` 自定义协议，不是普通
  TLS；不要直接向该端口发 ClientHello。
- Windows 已接入 Opus 双声道播放、音量/静音、限长缓冲、内带恢复和丢包隐藏；用户已确认声音正常；卡顿爆音的平滑处理仍待实机验收。
- 只实现了主控端（controller）；被控端（host）的桌面采集/编码还没接。
- 视频只解 H264；H265 需要额外的解码器。
- 键盘和鼠标均走 `CONTROL_DATA_CHANNEL` 裸 JSON，不再使用独立 TURN/PCK 输入通道。

## 实现范围

本项目采用原生 Rust 实现。早期评估过 Wine 路线，客户端组件中的 IDD、ViGEm 和输入驱动仍需与应用层功能区分。

Windows 支持前台串流专属的双向文本剪贴板（30KiB）、原生 VK 键盘输入、桌面绝对定位与游戏相对鼠标。
多显示器使用真实源 ID 与轨道绑定，标签支持拖动和按设备保存排序；默认保持远端显示器布局。
