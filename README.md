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
| M3b | 键盘输入 | ✅ CONTROL 通道 VK 注入已实测 |
| M4 | 官方 API：签名、扫码登录、设备列表、room/join | ✅ |
| M5 | 官方信令：Socket.IO (`X-NRD-AUTH`) + SDP/ICE | ✅ |
| M6 | GUI：设备列表、扫码登录、远程画面、全屏/断开 | ✅ |
| M7 | Windows x64 交叉构建 | ✅ 已在 KVM-WIN11 启动验证 |
| M8 | Windows D3D11VA 硬件解码 | ✅ Intel Arc B390 / AMD RX 7900 XTX 实测；Linux VA-API 待真机验证 |
| M9 | 输入与串流体验 | ✅ Windows 中键按住/拖动、浅色设备主页、RTP 重排/坏帧丢弃/恢复、最新帧呈现 |
| M10 | 自动档与重传恢复 | ✅ DecoderCap/自动画质、NACK/RTX、严格参考链恢复、可隐藏悬浮菜单；短测17–19Mbps |

> 现在已经能做到：连接官方被控端、连续看画面、用鼠标和键盘操作。剩余的是
> 文件等外围功能，以及原生采集后端（目前是占位）。

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
  uu.exe
  libstdc++-6.dll
  libgcc_s_seh-1.dll
  README.txt
  install.ps1
  version.json
```

正式安装目录统一为 `D:\SMB\uu-gui-demo`，桌面入口是 **UU Remote Rust**。
在分发包目录运行 `powershell -ExecutionPolicy Bypass -File .\install.ps1` 安装/升级；
旧 `rtx-auto`、`hud-recovery` 和 exe 备份会归档到 `%USERPROFILE%\uu-gui-archive`。
主页底部显示源代码版本号，悬停显示实际 exe 路径；`version.json` 记录该包的提交号与文件哈希。
开发端统一用 `python3 tools/windows/package.py` 构建并生成包，避免手动复制错版本。
旧C盘目录不再同步；安装器会将指向旧入口的桌面/开始菜单快捷方式迁到D盘，不终止正在运行的旧窗口。

首次启动会显示二维码，用手机扫码登录；左侧选择设备后点“连接”。

Windows 默认优先使用 D3D11VA，失败时回退到 OpenH264；设置 `UU_DECODER=sw` 可强制软解。
HUD 显示实际解码后端。离线验证可运行
`uu.exe probe --h264 sample.h264 --frame first-frame.ppm`，同时检查解码数量、尺寸和首帧图像。
当前硬解仍需 CPU 回读，并不保证比软解更快。最新进度见 [`handoff/README.md`](handoff/README.md)。

设备主页按参考图改成浅色布局；连接在独立串流窗口打开，设备主窗口保留，可继续连接其他设备。Ctrl+Alt+S 切换统计，
Ctrl+Alt+F / X 切换全屏串流，Ctrl+Alt+Q 退出；界面按钮只负责窗口最大化。串流窗口默认在目标显示器工作区80%内等比适配、居中，跨屏自动重新适配。Windows 路径中键使用实测的 `MK_MBUTTON=16`，支持按住拖动。
接收端支持短时 RTP 重排、NACK/RTX 和参考链恢复；FEC 尚未接入。
默认使用官方自动画质，默认保持远端分辨率；全屏悬浮菜单可用 Ctrl+Alt+M 隐藏/恢复。
控制中心 → 画质整合原画/超清/高清、自定义码率、30/60/90/120/144 FPS 和自定义 5–144 FPS。
菜单按官方参考使用黑点选中与灰色悬停，不显示常驻确认小字；默认保持远端显示器布局；软件设置可选UU虚拟屏独占，重新连接生效。顶部标签页与全屏菜单按真实屏幕列表切换，显示名称和主屏标记。
HUD 区分目标帧率、远端屏幕类型/刷新率和实际 Rx/De/Rd；视频更新不代表输入控制通道已就绪。
协议依据、当前边界和测试方法见 [`docs/frame-rate.md`](docs/frame-rate.md)。
恢复机制、卡顿原因和 Sunshine/Moonlight 对比见 [`docs/streaming-quality.md`](docs/streaming-quality.md)。
视频正常而输入SCTP长期connecting的DTLS接收竞态已复现并修补，用户已确认`f74fc9d`恢复键鼠，证据与验收范围见[交接17](handoff/17-2026-10-19-dtls-reader-sctp.md)。
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
  JSON**（先做 SimpleAction echo 握手）。已在 KVM-WIN11 上用低层键盘钩子
  验证 `VK` 注入、并用点击开始菜单验证鼠标。
- 被控端 TCP host candidate 在 STUN 后说的是 `0x04` 自定义协议，不是普通
  TLS；不要直接向该端口发 ClientHello。
- Windows 已接入 Opus 双声道播放、音量/静音、限长缓冲、内带恢复和丢包隐藏；用户已确认声音正常；卡顿爆音的平滑处理仍待实机验收。
- 只实现了主控端（controller）；被控端（host）的桌面采集/编码还没接。
- 视频只解 H264；H265 需要额外的解码器。
- 键盘和鼠标均走 `CONTROL_DATA_CHANNEL` 裸 JSON，不再使用独立 TURN/PCK 输入通道。

## 与前期调研的关系

`notes/` 记录了动手前的可行性调研：官方无 Linux 版、安装包载荷加密、以及
把官方 Windows 客户端放进 Wine 的几条既有路线（`uur`、`uu-remote-for-linux` 等）。
该路线最终卡在客户端 UI 主动退出上，因此转向本仓库的**原生重写**。
这些笔记仍有价值：它们给出了客户端组件清单与内核驱动分割线（IDD/ViGEm/uuycinput）。

日常使用 demo 的快捷键、80% 跨屏窗口、音频、全屏键盘捕获、持久设置和鼠标模式见[交接18](handoff/18-2026-10-19-basic-demo.md)。

Windows已接入前台串流专属的双向文本剪贴板（30KiB），窗口／全屏普通输入统一使用原生VK；详见交接18。

多显示器协议、两种布局模式和验收范围见[交接19](handoff/19-2026-10-19-multi-display.md)。
未绑定屏幕激活、轨道负值修复与D盘安装迁移见[交接20](handoff/20-2026-10-19-display-binding-install.md)。
