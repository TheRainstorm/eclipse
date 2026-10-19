# 本轮交接：修复远端视频冻结 + 打通键盘/鼠标输入

更新时间：2026-10-17（深夜）
上一份交接：`HANDOVER-RUST-TURN-2026-10-17.md`（TURN/PCK 实现，保留）；
更早的协议背景以 `HANDOVER.md` 为准。本文只记录本轮内容。

## 0. 一句话现状

原生 Rust 客户端已经可以**连接官方被控端 → 连续看 1080p 画面 → 用鼠标和键盘操作**。
之前两个卡点（画面只出首帧冻结、键鼠注不进去）本轮都已解决并在实机验证。
剩下的都是外围功能（音频/剪贴板/文件/原生采集），不再是主链路阻塞。

## 1. 本轮完成

### 1.1 远端视频连续解码（此前只出首帧、约 6 FPS 冻结）

**根因**：网易被控端编码器为了码率控制会主动跳帧（`frame_num` 递增但帧不发），
而 SPS 里 `gaps_in_frame_num_value_allowed_flag = 0`。ffmpeg 容忍这种不规范，
OpenH264 严格照做：一次 frame gap 就报 `dsRefLost` / `dsNoParamSets`，永久失步到
下一个 IDR，于是首帧之后级联失败、画面冻结。

**修复**（`crates/uu-media/src/h264.rs`）：

1. 解码前改写每个 SPS 的 `gaps_in_frame_num_value_allowed_flag = 1`
   （`patch_sps_allow_gaps`，含 RBSP 位级解析 + emulation-prevention 还原/重建）。
2. `DecoderConfig` 用 `Flush::NoFlush`（默认每次解码后 flush 会砸掉参考帧列表）。
3. 打开 error concealment：`raw_api().set_option(8 /* ERROR_CON_IDC */, 1)`。

**验证**（ares → KVM-WIN11，release）：
- 修复前：30s 内 800+ 次 `Native:16/18`，只解出 30 帧。
- 修复后：30s 解出 600+ 帧，`decode error = 0`，画面持续更新。

### 1.2 键盘/鼠标输入打通

**根因**：输入不是我方此前假设的几种形式。真实协议是：

1. 官方主控端创建 **5 个数据通道**，名字必须是
   `CONTROL_DATA_CHANNEL` / `TEXT_DATA_CHANNEL` / `STREAMER_DATA_CHANNEL` /
   `FILE_DATA_CHANNEL` / `BINARY_DATA_CHANNEL`。我们原来用 `webrtc-datachannel`，
   被控端完全不处理输入。
2. 通道打开后要做 **SimpleAction echo 握手**。被控端会持续重发 36 字节的
   echo 请求（`Message{1:seq,3:{4:features}}`，`action` 缺省为 0），主控端必须回
   `Message{3:{1:1,2:{"seq":请求seq},4:features}}`，握手完成后才接受输入。
3. 输入是 **CONTROL 通道上的裸 JSON**（没有 28 字节 PCK 头）：
   - `{"action":"mouse_move_absolute","abs_x":0..1,"abs_y":0..1}`
   - `{"action":"mouse_move_relative","delta_x":..,"delta_y":..}`
   - `{"action":"mouse_press"/"mouse_release","button":1..}`
   - `{"action":"mouse_scroll","delta_x":..,"delta_y":..}`
   - `{"action":"kbd_press"/"kbd_release","key":<Windows VK>}`

**修复**：
- 新增 `crates/uu-core/src/control_wire.rs`：免依赖 protobuf
  varint 编解码 + echo 请求/响应 + 请求检测（含 3 个单测）。
- `crates/uu-core/src/media.rs`：创建 5 个通道；CONTROL 打开时发我方 echo_request，
  收到被控端 echo_request 立即回 echo_response。
- `crates/uu-core/src/controller.rs`：GUI/CLI 仍产出 PCK 帧，这里剥掉 28 字节头和
  尾部 `03 00 01 00`，把 JSON 直接发到 CONTROL；删除了旧的 PCK capability hello。

**验证**（ares → KVM-WIN11）：
- 键盘：KVM 低层钩子 `kbdhook.exe` 记录到 `vk=90`（`Z`）按下/抬起
  （此前只有连接/断开的合成 `VK_F24`）。
- 鼠标：`uu mouse-move --x 0.003 --y 0.998 --click` 让被控端 Win11 开始菜单弹出。

### 1.3 Windows GUI 包

- `dist/uu-gui-win64/uu-gui.exe`
  sha256 `774fc1295c7a9d71e32f503608af287b92f8c577594a2611e7deb63aea3c38a4`。
- 已部署到 x358h：`C:\Users\yfy\uu-gui-demo\`（DLL 与 README 未变）。

### 1.4 文档

- 新增博客 `docs/blog-uu-remote-input.md`（本轮攻坚过程复盘）。
- 更新 `README.md`、`HANDOVER.md`、`dist/uu-gui-win64/README.txt` 的状态。

## 2. 代码改动清单

| 文件 | 改动 |
|---|---|
| `crates/uu-media/src/h264.rs` | SPS gaps 标志修补、NoFlush、error concealment；新增测试 |
| `crates/uu-media/examples/dec_stream.rs` | 新增：把 Annex-B 流喂给 OpenH264 的离线诊断对照工具 |
| `crates/uu-core/src/control_wire.rs` | 新增：CONTROL protobuf + echo 握手 |
| `crates/uu-core/src/media.rs` | 5 个数据通道 + echo 收发；解码诊断开关 `UU_MEDIA_DEBUG`/`UU_DUMP_H264` |
| `crates/uu-core/src/controller.rs` | 输入改为 CONTROL 裸 JSON；`pck_payload_json` 去 PCK 头尾 |
| `crates/uu-cli/src/main.rs` | 新增 `uu mouse-move` 诊断命令 |
| `docs/blog-uu-remote-input.md` | 新增 |

> 旧的 TURN/PCK 代码（`turn.rs`、`pck.rs`、controller 里的后台 TURN 任务）保留未删。
> 现在输入不再依赖它，那个后台任务只是空转 + 输出误导性的
> 「键盘通道已就绪 (TURN/PCK)」状态，建议下一轮清理。

## 3. 快速开始 / 复现

```bash
# 依赖与登录态：见 HANDOVER.md。state.json 已在本机和 x358h 存在。
cargo test -p uu-core -p uu-media          # 16 + 14 全绿

cargo run --bin uu -- devices              # 设备列表（需有效 token）
cargo run --release --bin uu -- control <device-id>          # 看画面（30s）
cargo run --release --bin uu -- kbd-test <device-id> --text Z  # 键盘联调
cargo run --release --bin uu -- mouse-move <device-id> \
    --x 0.003 --y 0.998 --kind 15 --channel 0 --click          # 点开始菜单自检
```

Windows GUI：`dist/uu-gui-win64/`（或 x358h 的 `C:\Users\yfy\uu-gui-demo\`），
双击 `uu-gui.exe` → 扫码/已登录 → 选设备 → 连接。

## 4. 验证结果一览

| 验证项 | 结果 |
|---|---|
| `cargo test -p uu-core` | 16 passed |
| `cargo test -p uu-media` | 14 passed |
| 真实会话连续解码 | 30s 600+ 帧，0 错误 |
| `kbd-test ... --text Z` → `kbdlog.txt` | `vk=90` 按下/抬起 |
| `mouse-move ... --click`（开始按钮） | 被控端开始菜单弹出 |
| Windows GUI 包 | 已重建并部署到 x358h |

## 5. 已知问题 / 下一步

- **清理死代码**：删除 controller 里空转的 TURN/PCK 后台任务；
  否则日志里的「键盘通道已就绪 (TURN/PCK)」会误导。
- **鼠标手感**：GUI 目前在 hover 时逐帧发 `mouse_move_absolute`；可评估改用
  `mouse_move_relative`（官方游戏场景用相对移动）。滚轮/拖拽已接。
- **心跳**：官方可能在 CONTROL 上需要周期性 `{"heartbeat":"kbd_heartbeat"}`；
  短会话未验证是否需要，长会话建议补上。
- **音频（Opus）**未接入；**剪贴板、文件传输、终端**未实现。
- **原生采集后端**（PipeWire / X11 / WGC）仍是占位，只有 `synthetic`。
- **H.265** 未解；**硬件解码**（VA-API / DXVA）未接。
- **NACK/RTX/FEC** 未接入，弱网下的表现未验证。
- 多显示器切换稳定性未系统验收。

## 6. 环境与工具（本轮补充）

沿用 `HANDOVER.md` 第 6 节的机器表。本轮新增的验证工具：

```
KVM-WIN11:
  C:\Users\yfy\kbdhook.exe    低层键盘钩子 → C:\Users\yfy\kbdlog.txt（可靠）
  C:\Users\yfy\mousehook.exe  低层鼠标钩子 → C:\Users\yfy\mouselog.txt
                              （本轮未成功捕获注入事件，验证鼠标请改看点开始菜单）
  C:\Users\yfy\kvmshot.ps1    截图（WMI 启动的会话截不到真实桌面，慎用）
```

- ares→KVM：`ssh -p 2202 yfy@192.168.35.6`；CPU 上直接看远端视频
  `/tmp/uu_rust_latest.ppm`（每 30 帧覆盖）是最可靠的观测手段。
- ares→x358h（GUI）：`ssh -p 2202 yfy@192.168.35.200`。

## 7. 风险 / 合规

- `UU_FORCE_JOIN=1`（controller 默认 `force=true`）会踢掉官方会话；测试前确认可独占被控端。
- 不随仓库分发网易二进制；建议用小号测试，逆向仅用于互操作。
