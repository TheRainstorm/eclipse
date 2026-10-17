# 本轮交接：串流专用 UI + 码率控制调查（直连/中继判定）

更新时间：2026-10-18（凌晨）
上一份交接：`HANDOVER-RUST-INPUT-2026-10-17.md`（输入打通）。
协议/环境背景沿用 `HANDOVER.md` 与 `docs/vision.md`。本文只记录本轮内容。

## 0. 一句话现状

原生 Rust 主控端（Linux ares → 官方 Windows 被控端 KVM-WIN11）可稳定看
**1080p@30fps** 画面、键鼠可用、UI 已改成**串流模式**（无设备列表、顶部一行控制条、
全屏只剩画面）。**唯一未解卡点：码率**——实测约 4.5 Mbps 封顶，已排除中继因素
（局域网直连），需用 Frida 抓官方主控端报文定位控制字段；抓包方法已固化到
`docs/official-client-capture.md`。

## 1. 本轮完成（已提交、已部署）

### 1.1 串流专用界面（回应「全屏=串流画面本身，不要左边显示其他东西」）

`crates/uu-gui/src/main.rs`：

- 连接后进入**串流模式**：**不再渲染左侧设备列表与底部栏**，视频铺满窗口。
- 顶部**仅一行控制条**：`串流 · 状态点` + 右侧 `分辨率·fps / 显示器 / 码率 / HUD /
  全屏 / 断开`。串流态标题精简为小字「串流」。
- **Ctrl+Alt+F 全屏**：全屏时**只画画面**（顶栏/侧栏/底栏全部跳过）。
- 默认窗口尺寸改为 `1120x680`（原 `1280x800` 在 125% DPI 屏上超出屏宽、右侧控件
  点不到）。*注意：`with_maximized(true)` 试过，窗口会开到终端后面导致截图看不到，
  已回退。*
- 截图验证：`/tmp/uu_demo7.png`（画布铺满 + HUD `src 30.0 / dec 30.0`，内容为用户
  在 KVM 上跑的高刷噪点页）。

> 尚未做成**独立第二窗口**（egui 多 viewport）。当前是「同一窗口切换成串流模式」，
> 视觉目标（无列表、一行控制、全屏仅画面）已达成。若要真正的独立窗口见「下一步」。

### 1.2 断开清状态

断开时发 Socket.IO `release` + 离开房间 + 关闭连接（提交 `b4e592f`），避免官方客户端
仍显示「已连接」。

### 1.3 主动发送 REMB 带宽估计

`crates/uu-core/src/media.rs`：周期 RTCP 任务里新增 `ReceiverEstimatedMaximumBitrate`
（`UU_BITRATE` Mbit/s，默认 30）。用于向宿主声明可用带宽。
**实测对官方 Windows 被控端无效**（码率仍 ~4.5M），保留，对其它宿主可能有用。

## 2. 码率调查的结论与证据（重要）

### 2.1 是局域网直连，不是网易中继（已用抓包证伪）

`ares(192.168.35.10) → KVM(192.168.35.6)` 串流期间 `sudo tcpdump`：

```
    59 In 192.168.35.6         <- 媒体全部来自内网 KVM
     1 Out 192.168.35.10
```

**没有任何发往网易中继 `112.28.x.x` 的媒体流量**。ICE 候选里虽同时有 host/srflx/relay，
但选中的是内网直连对。所以 **~4.5Mbps 不是中继限速**，是宿主编码器侧。

### 2.2 现有码率/画质档位宿主基本不采纳

- `UU_QUALITY` 1..5 只让 q1 掉到 0.5M，q2/q3/q4/q5 都 ~4.5M（参考项目里 q2 应≈2M、
  q4≈20M，说明官方 Windows 被控端**没有按参考项目的语义响应我们的字段**）。
- `UU_BITRATE`（field8，自定义 Mbit/s）单发/配合 q6 都无效（曾掉到 0.5M）。
- 新增的 REMB（30M）也无效。

→ 结论：**码率/画质偏好很可能藏在官方主控端 `control` 消息那段 177 字节的 `app_data`
protobuf 里**（我们只发了最小版 device_id+version，缺这些字段）。靠猜字段不现实，需抓包。

### 2.3 参考项目（`VallierDev/uu-remote-linux`）能参考到什么

- 它是**被控端**（host）；`src/capture_setting.rs` 负责**解析**请求。
- 它的 **viewer 确有**画质/码率选择，字段映射为：
  `CaptureSettingRequest{1:fps档(1=30/2=60/3=90/4=144), 2:质量档(1 fast..5 auto/6 custom),
  8:自定义 Mbit/s, 18:显式fps}`。我们**已按此实现**（`control_wire.rs`），但与官方
  Windows 被控端行为不一致 → 仍需官方报文确认。

## 3. 进行中：Frida 抓官方主控端码率报文

目标机 **x358h**（`192.168.35.200`），官方主控端 `D:\Programs\GameViewer\bin\GameViewer.exe`
（本轮 PID 49608）。Frida 17.24 可用，Python：
`C:\Users\yfy\AppData\Local\Programs\Python\Python311\python.exe`。

### 抓包方法（已定型，详见 `docs/official-client-capture.md`）

**唯一正确的钩子点**：`streamer.dll` 内联 OpenSSL 的写/读函数，硬编码 RVA
`0xdd7f30`（写）/ `0xdd7690`（读）。脚本 `C:\Users\yfy\wire2.js` + `wire2.py`
（等价于当年的 `wirecap.js`），输出 `wire2.jsonl`。

- ❌ 钩 `libssl-3-x64.dll` 导出的 `SSL_write`（`sslcap2.js`）**抓不到**——官方数据走
  streamer.dll **静态链接**的 OpenSSL，不经过 libssl 导出。这一轮白折腾了一回。
- ❌ `plaincap.js` 拿到的是 **TLS 密文**（socket 层，能看到 `17 03 03 00 41` 记录头）。
- ✅ 明文是 **PCK 帧**：`b[:3]=="PCK"`、`b[8]==0x51`、`seq=b[9:13]BE`、
  `channel=b[17:19]LE`、`kind=b[20:24]LE`、`payload=b[28:]`（JSON 或 protobuf）。

**脱离 SSH 会话启动**（否则子进程随 SSH 断开被杀，之前几次"空跑"就是这个原因）：
用 `Invoke-CimMethod -ClassName Win32_Process -MethodName Create` 跑
`cmd /c C:\Users\yfy\run_wire2b.bat`（`wire2.py <pid> 300 <out>`）。
不要用 `/RU SYSTEM` 计划任务（Frida 附加会话进程失败）。

### 当前状态 / 结论

- 抓包链路已**验证可用**（`wire2` 挂钩成功，`writeRva=0xdd7f30`/`readRva=0xdd7690`）。
- **码率报文至今没抓到**：4 份历史抓包（`/tmp/wirecap*.jsonl` 等）8683 个 PCK 帧
  **全是输入 JSON**；本轮几次抓包窗口内官方客户端**没在推流**（只有保活帧
  `81 01 32` / `81 81 99 00 f4 0b aa`），所以没触发码率报文。

### 待你配合的一步

1. 官方客户端**连上 KVM-WIN11** 并看到画面；
2. 在远端**动一下鼠标**（确认 PCK 在流）；
3. **抓包窗口内切码率几档**（尤其 30M）。
4. 我读 `wire2.jsonl`，解析非输入类 PCK 帧（protobuf），定位码率/画质字段。

## 4. 构建 / 运行 / 测试

### 构建

```bash
cargo build --workspace                 # 本机(Linux)全量
cargo test  -p uu-media -p uu-core      # 单测
cargo build --release -p uu-gui --target x86_64-pc-windows-gnu   # Windows GUI
cp target/x86_64-pc-windows-gnu/release/uu-gui.exe dist/uu-gui-win64/uu-gui.exe
```

### 关键环境变量

| 变量 | 作用 |
|---|---|
| `UU_FPS` | 请求帧率档 `1/2/3/4`=30/60/90/144（默认 2；**官方被控端实测封顶 30**） |
| `UU_QUALITY` | 质量档 `1..6`（1 fast … 5 auto / 6 custom），默认不发送 |
| `UU_BITRATE` | 自定义/REMB 码率（Mbit/s），默认 30 |
| `UU_HUD` | `0` 关闭 HUD（默认开，或运行时按 F1） |
| `UU_VSYNC` | `0` 关垂直同步（默认开，无撕裂） |
| `UU_FORCE_JOIN` | `1` 强占会话（会踢掉官方会话，测试前确认可独占） |
| `UU_AUTOCONNECT` | GUI 启动即连（**只接受明确的 device_id/alias**，不再 first-online） |
| `UU_HOLD_SECS` | CLI `mouse-move`/`kbd-test` 保持秒数 |

### 运行

```bash
# 控制器路径（有握手/stats，勿用 legacy `uu control`）
UU_FORCE_JOIN=1 UU_HOLD_SECS=16 ./target/release/uu mouse-move aeawvmepembcpca5 --x 0.5 --y 0.5
RUST_LOG=webrtc=debug ./target/release/uu control aeawvmepembcpca5     # 仅 ICE/信令调试
./target/release/uu probe        # 查看解码后端
```

GUI（x358h）：`C:\Users\yfy\uu-gui-demo\uu-gui.exe`。本轮用 `/tmp/uu_demo.ps1`、
`/tmp/reg2.ps1`（经 SSH 上传到 x358h）拉起来并截图回传（base64）。

## 5. 设备与访问

| 机器 | 角色 | 地址 / 连接 |
|---|---|---|
| ares | 开发机（Linux 主控端） | 本机 |
| KVM-WIN11 `aeawvmepembcpca5` | 官方被控端（**唯一测试机**） | `ssh -p 2202 yfy@192.168.35.6` |
| x358h | 官方主控端 + Windows GUI 演示 | `ssh -p 2202 yfy@192.168.35.200` |

- **绝不连接 `FYYUAN-5NET3-53`。**
- 官方被控端对桌面封顶 **30fps**；静态内容默认 ~21fps、~5Mbps 上限。
- `target/`、`artifacts/` 已 gitignore。当前 HEAD：`05f2e0d`。

## 6. 已知问题 / 下一步

1. **码率（主卡点）**：按 `docs/official-client-capture.md` 抓官方主控端报文（见 §3），
   定位真正字段 → 打通 30Mbps 局域网。
2. **独立串流窗口**：若要真正的第二 OS 窗口（egui `show_viewport_immediate`），需处理
   多 viewport 的输入/全屏事件路由；当前「同窗串流模式」已满足视觉诉求。
3. **窗口/DPI**：x358h 125% 缩放下已用 1120x680 规避出屏；如需更稳可做「按显示器尺寸
   自适应开窗」。
4. **Windows 硬件解码**：计划进程内 Media Foundation（默认开、软件 OpenH264 兜底）；
   Linux 侧 VA-API。尚未开工。
5. **剪贴板 / 多显示器切换 / 鼠标自由切回本机**（Sunshine/Moonlight 痛点项）未做。
6. 音量/音频（Opus）、文件传输、原生采集后端仍是占位。
7. 长会话心跳（CONTROL `kbd_heartbeat`）未验证是否需要。

## 7. 本轮提交与文件

- `597d97d` feat(gui): 串流模式（隐藏设备列表/一行控制条/全屏仅画面）+ 发送 REMB 带宽估计
- `b4e592f` feat(ui): Ctrl+Alt+F 全屏、断开时释放会话、码率/质量档位选择
- 相关文件：`crates/uu-gui/src/main.rs`（串流模式/全屏/尺寸）、
  `crates/uu-core/src/media.rs`（REMB）、`crates/uu-core/src/controller.rs`（`UU_*`）、
  `crates/uu-core/src/control_wire.rs`（`capture_setting_request` 字段 1/2/8/18）。
- 新增文档：**`docs/official-client-capture.md`**（官方客户端明文抓包方法，必读）。
- x358h 抓包产物：`C:\Users\yfy\wire2.js` / `wire2.py` / `run_wire2b.bat`
  （当前推荐）；旧件 `wirecap.*`、`plaincap.*`、`sslcapture*.*`、`sslcap2.*`。
- 参考协议（本机 `/tmp`）：`/tmp/uul_capture_setting.rs`、`/tmp/uuv_wire.rs`、
  `/tmp/uul_control_protocol.rs`、`/tmp/uuv_rtc.rs`、`/tmp/r_capture_setting.rs`、
  `/tmp/r_desktop.rs`。

## 8. 风险 / 合规

- 逆向仅用于互操作；不随仓库分发网易二进制；建议用小号测试。
- `UU_FORCE_JOIN=1` 会踢掉官方会话。
- （遗留）此前有一次来自 `10.0.31.1`、使用同一把 key 的 SSH 登录，非 ares
  (`192.168.35.10`)，建议核实 key 安全。
