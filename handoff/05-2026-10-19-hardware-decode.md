# 本轮交接：原生硬件解码（Windows D3D11VA / Linux VA-API）

更新时间：2026-10-19
上一份交接：`04-2026-10-18-bitrate.md`（码率已解决，1080p30 / 55 Mbps）。
协议与历史背景沿用 `01-base-handover.md`。本文只记录本轮增量。

> 状态：**代码完成、可编译、软解路径已实测；两块硬件路径都还没有在真实 GPU 上跑通验证。**
> 下一步的第一件事就是拿 `uu.exe probe --h264` 在 x358h 上确认 D3D11VA 真的接管了解码。

## 0. 一句话现状

解码器从「直接 new 一个 OpenH264」改成 **硬件优先 + 运行时回退** 的选择器：
Windows 走 Media Foundation H.264 MFT + D3D11 设备管理器（D3D11VA），
Linux 走 VA-API（cros-codecs stateless）。两条硬件路径都输出带行跨距的 NV12，
core 层用新的 `frame_to_rgba` 统一上屏；硬件被驱动「静默拒绝」时
`FallbackDecoder` 会自动降级到 OpenH264，并在 HUD 上写明降级原因。

## 1. 代码结构（本轮新增/改动）

### 1.1 `crates/uu-media`（核心）

| 文件 | 内容 |
|---|---|
| `src/bitstream.rs`（新，385 行） | 从 `h264.rs` 抽出的 H.264 码流工具：`patch_sps_allow_gaps`（网易丢帧必需）、`h264_dimensions`、ue(v)/位读取。**不再 feature-gated**，软解与硬解共用。 |
| `src/decoder.rs`（新，294 行） | `DecoderKind`（`UU_DECODER=auto|sw|hw` 解析）、`FallbackDecoder`（硬件失败自动降级）、`NamedDecoder`（把「硬件不可用：原因」挂到实例上，供 HUD 显示）。 |
| `src/hw/mod.rs`（新） | 平台后端选择：`create_hardware_decoder()`、`backend_label()`；两个后端都由 feature 控制。 |
| `src/hw/thread.rs`（新，133 行） | `Worker`：硬件解码独占一个 OS 线程。原因见 §1.2。 |
| `src/hw/d3d11va.rs`（新，883 行） | Windows：`IMFTransform` H.264 MFT + `IMFDXGIDeviceManager`；D3D11 staging 回读 NV12。 |
| `src/hw/vaapi.rs`（新，342 行） | Linux：`cros-codecs` stateless H.264 + VA-API display/GBM surface pool；按 NAL 逐个喂、按事件驱动重试。 |
| `src/convert.rs` | 新增 `frame_to_rgba`（按格式分派）、`nv12_to_rgba`、`bgra_to_rgba`、`i420_to_rgba_strided`（支持行跨距）。原 `i420_to_rgba` 保留为紧凑包装。 |
| `src/codec.rs` | `VideoDecoder` 增加 `backend_name()`（默认实现，保持兼容）。 |
| `src/lib.rs` | `software_decoder()`（原 `default_decoder` 的软解部分）、新的 `default_decoder()`（硬件优先 + 回退）、`BACKEND` OnceLock 缓存实际后端名。 |

### 1.2 为什么硬件解码要独占线程

`crates/uu-core/src/media.rs` 的解码循环是 `tokio::spawn` 的任务，`.await` 之后可能
落在任意 worker 线程上。Media Foundation 要求每个线程有 COM apartment，D3D11 的
immediate context 与 VA-API display/surface 也都是线程亲和（除非额外开多线程保护）。
所以 `hw::Worker` 用一个专用线程完成 **创建 → 解码 → 销毁**，`VideoDecoder` 只是一个
发消息的句柄。这同时保证驱动状态在原线程释放。

### 1.3 调用方改动

- `crates/uu-core/src/media.rs`：改用 `default_decoder()`；启动时打印后端；每 2 秒把
  **当前**后端名写回 `MediaStats`（降级后 HUD 会变）；送 GUI 的像素改用 `frame_to_rgba`。
- `crates/uu-core/src/selfloop.rs`：显式用 `software_decoder()`。自环是编解码质量门禁，
  不应该依赖机器上有没有 GPU。
- `crates/uu-gui/src/main.rs`：HUD 第一行显示短标签（`HW d3d11va (MF)` / `HW vaapi` /
  `SW openh264`，降级时追加「hw 已回退 / hw 不可用」），第二行显示完整后端描述（含 GPU 名
  或失败原因），HUD 背景宽度按最长行自适应。
- `crates/uu-cli/src/main.rs`：`uu probe --h264 <file>` 离线自测（见 §3.2）。

### 1.4 依赖与特性

```toml
[features]
default = ["h264", "d3d11va"]
d3d11va = ["dep:windows"]        # 仅 Windows 目标生效，默认开启
vaapi   = ["dep:cros-codecs", "dep:cros-libva"]   # 仅 Linux，需显式开启
```

- Windows 依赖 `windows 0.62`（Media Foundation / D3D11 / DXGI），mingw 交叉编译已验证。
- Linux 用 `cros-codecs 0.0.6`（feature `vaapi`）+ `cros-libva 0.0.12`，构建期需要
  `libva-dev`、`libgbm-dev`、`libdrm-dev`，并且 cros-libva 的 bindgen 需要 clang。

## 2. 设计要点与已知取舍

1. **SPS 修补在两条硬件路径上都保留。** 网易编码器丢帧但 SPS 的
   `gaps_in_frame_num_value_allowed_flag=0`，MFT/VA-API 与 OpenH264 一样会因此失步。
   两个后端都在 `decode()` 前跑 `patch_sps_allow_gaps`。
2. **回退是运行时的，不是启动时的。** 驱动可能「所有调用都成功但永远不出帧」，所以
   `FallbackDecoder` 的两条触发条件：
   - 连续 3 次 `Err`；
   - 已喂 120 个访问单元（30 fps 下约 4 秒）仍 0 输出。
   降级后本实例永久走软解，并把原因写进 `backend_name()`。
3. **D3D11VA 只接受同步 MFT。** 异步 MFT 需要事件循环，本轮未实现，直接跳过并尝试下一个候选。
   `MF_SA_D3D11_AWARE` 在 MFT 不支持 `GetAttributes` 时（微软内置解码器就是）不会硬性拒绝，
   改由 `MFT_MESSAGE_SET_D3D_MANAGER` 的返回值判断。
4. **拒绝「假硬件」。** 若解码器收了 D3D 设备管理器却连续返回系统内存帧（>30 帧），
   `read_surface` 直接报错，让上层降级到 OpenH264——那才是更快的软解。
5. **仍然有 CPU 回读。** GUI 是 egui/glow，纹理上传要 RGBA，所以硬件帧
   （D3D11 staging 纹理 / VA-API surface map）回读后由 `frame_to_rgba` 转 RGBA。
   真正的零拷贝上屏（NV12 纹理直接给 GPU）是后续工作，届时 `RawFrame.gpu` 才会真正用上。
6. **VA-API 每次 `decode` 只吃一个 NAL**（cros-codecs 的约定），
   `CheckEvents` / `NotEnoughOutputBuffers` 都按「排空事件后重试同一段字节」处理，
   连续 3 次无进展才报错，避免死循环。

## 3. 验证结果（真实输出）

### 3.1 单元测试

```text
cargo test --workspace                     # 64 passed, 0 failed
cargo test -p uu-media --features vaapi    # 29 passed（含 VA-API 行重排/NV12 打包测试）
```

新增测试覆盖：SPS 修补（含「不改动 NAL 长度」「保留后续 NAL」）、尺寸解析、
回退策略（健康硬件直通 / 连续错误降级 / 静默硬件降级 / 单次错误不降级）、
NV12/I420/BGRA → RGBA（含行跨距与 UV 去交错）、VA-API 平面重排、访问单元切分、
`first_mb_in_slice`、MF `FRAME_SIZE/FRAME_RATE` 打包。

### 3.2 离线自测：`uu probe --h264`

```bash
uu probe --h264 artifacts/h264/uu-1080p.h264          # 默认（硬件优先）
UU_DECODER=sw uu probe --h264 artifacts/h264/uu-1080p.h264
```

样本来自 KVM-WIN11 的真实 UU 码流（1,971,138 字节，540 个访问单元，1080p30）。
ares 上软解实测：

```text
decoder           openh264 (software, in-process)
input             ... (1971138 bytes, 540 access units)
decoded           539 frames in 2927 ms (184.2 fps), I420
decoder after     openh264 (software, in-process)
```

0 错误。注意这条命令也会打印 `decoder after`，因为硬件中途降级只体现在名字里。

### 3.3 Windows 交叉编译

```bash
cargo build --release -p uu-gui -p uu-cli --target x86_64-pc-windows-gnu
```

产物（**尚未上传 x358h**）：

| 文件 | 大小 | SHA-256 |
|---|---|---|
| `target/x86_64-pc-windows-gnu/release/uu-gui.exe` | 24,583,387 | `6dff56ca40da3736ee8a85b3f7ade067c015d4c37fb567432de4922e0b2370c8` |
| `target/x86_64-pc-windows-gnu/release/uu.exe` | 27,800,353 | `2a1c9842db2501cd4516c8d36df26393b2f4de8d34ea392f0165ac5cfffa1c5d` |

### 3.4 Linux VA-API：在 ares 上无法验证

ares 的 `/dev/dri/renderD128` 是 NVIDIA GP104（`DRIVER=nvidia`，nouveau 驱动），
实测：

```text
UU_DECODER=hw uu probe --h264 artifacts/h264/uu-1080p.h264
selected decoder: openh264 (software, in-process)
  [hardware unavailable: backend failure: no render node decodes H.264 with VA-API:
   /dev/dri/renderD128: VA display: call to vaInitialize failed: unknown libva error]
```

回退链路工作正常；但 **VA-API 解码本身没有跑通过**，需要 Intel/AMD 的 VA-API 驱动
（laptop-arch 或另装机）才能验证。

### 3.5 Windows D3D11VA：未验证

本轮结束时 `ssh -p 2202 yfy@192.168.35.200` 连接失败（`rc=255`，主机不可达），
因此 D3D11VA 只有编译验证，没有运行验证。

## 4. 本轮对环境做的改动

- ares 安装了 `libva-dev`、`libgbm-dev`、`libdrm-dev`、`clang`（VA-API 构建依赖）。
- `sudo usermod -aG render yfy`（当前 shell 用 `sg render -c ...` 生效；重新登录后永久生效）。
- 新增 `artifacts/h264/uu-1080p.h264`（已被 `.gitignore` 忽略，仅本机测试样本）。

## 5. 下一步（按优先级）

1. **x358h 上验证 D3D11VA**：把新的 `uu.exe` 和样本传过去，运行
   `uu.exe probe --h264 uu-1080p.h264`。期望看到
   `d3d11va (Media Foundation hw: <GPU 名>)`，并对比软解/硬解的 fps。
   若输出里出现 `hardware unavailable` 或 `abandoned`，原因会直接写在括号里。
2. **HUD 对比**：x358h 连 KVM-WIN11，`UU_QUALITY=6 UU_BITRATE=55`，看 `decode_ms`
   是否从软解的 ~20 ms 明显下降；同时看 `errors` 是否为 0。
3. **Linux 真机**：在有 VA-API 驱动的机器上
   `cargo run -p uu-cli --features vaapi -- probe --h264 artifacts/h264/uu-1080p.h264`。
4. **若是异步 MFT 导致回退**：实现 `IMFMediaEventGenerator` 事件循环（`MF_EVENT_FLAG_*`、
   `METransformNeedInput` / `METransformHaveOutput`），这是当前 D3D11VA 唯一结构性的缺口。
5. **零拷贝上屏**：让 `frame_to_rgba` 之上的 GUI 直接吃 NV12 纹理，省掉 staging 回读。
6. **分发**：`dist/uu-gui-win64/uu-gui.exe` 仍是旧版，需要替换并补 `uu.exe`；
   上传前先核对上表的 SHA-256。

## 6. 仓库状态提醒

- 全部改动 **未提交**。`handoff/` 下的 01–04 是 `git mv`（保留历史）后的文件，
  05 与 `handoff/README.md` 是新增。
- 码率相关的改动（`control_wire.rs`、`controller.rs`、GUI 默认 55 Mbps）也仍未提交。
- `target/` 约 9 GB，注意磁盘。
