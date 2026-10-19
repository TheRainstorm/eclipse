# 本轮交接：Windows 硬解实机验收与画面尺寸修复

更新时间：2026-10-19。承接 `05-2026-10-19-hardware-decode.md`，本轮只记录增量。

## 1. 结论与机器地址

**Windows D3D11VA 已在两块真实 GPU 上跑通，均未回退、无解码错误。**

- x358h：用户更新 SSH 地址为 **`192.168.34.3:2202`**，用户 `yfy`。
  旧 `192.168.35.200` 已失效；GPU 为 Intel Arc B390，驱动 `32.0.101.8356`。
- KVM-WIN11：`192.168.35.6:2202`，用户 `yfy`；GPU 为 AMD RX 7900 XTX，
  驱动 `32.0.31041.1004`。它既是官方被控测试机，也可独立运行离线解码验证。
- 本轮只连接了指定的 KVM-WIN11 `aeawvmepembcpca5`；GUI 验证显式设置
  `UU_FORCE_JOIN=0`，没有强占官方会话。
- laptop-arch `192.168.35.100` 本轮未能 SSH 连接。Linux VA-API 仍无真机验收。

## 2. 修复与自测增强

### D3D11VA 可见尺寸

实测旧代码解码 1080p 样本时，输出 `Nv12 1920x1088 stride 1920`，软解则为
`1920x1080`。旧实现把纹理分配尺寸当成了可见画面尺寸。

新增 `crates/uu-media/src/hw/surface.rs`：按 SPS 的可见尺寸复制 Y/UV，去掉纹理
右侧和底部对齐；**UV 起点仍按分配高度计算**，不能直接把 1088 改成 1080 后整段复制。
输出现在为紧凑 NV12，`width=1920, height=1080, stride=1920`。
新增测试覆盖底部/行尾填充、UV 偏移、无效尺寸及截断数据。

### 解码输入与资源

- D3D11VA / VA-API 都移除了 `pending.pop_front()` 后立即返回的分支：
  即使有重排序输出积压，也必须先喂本次新访问单元，否则会悄悄丢掉压缩帧。
- `MFT_OUTPUT_DATA_BUFFER.pSample` 是 `ManuallyDrop`；现在在所有 `ProcessOutput`
  返回路径取回所有权，包括 `NEED_MORE_INPUT` / `STREAM_CHANGE`，避免引用泄漏。
- MF 输出时间戳从 100 ns 单位除以 10，再放入本项目微秒 `Timestamp`。

### `uu probe`

```powershell
.\uu.exe probe --h264 uu-1080p.h264 --frame first-frame.ppm
$env:UU_DECODER='sw'
.\uu.exe probe --h264 uu-1080p.h264
Remove-Item Env:UU_DECODER
```

- 前三帧增加尺寸和 stride 输出。
- `--frame FILE` 把首帧经生产路径 `frame_to_rgba` 转换后写成 RGB PPM，便于检查颜色和裁剪。
- 检查帧缓冲完整性；零出帧或有解码错误返回非零退出码。
- 实测仅 AUD 的样本返回 1；单独使用 `--frame` 缺少 `--h264` 返回 2。

## 3. 离线实测

真实 UU 样本：`artifacts/h264/uu-1080p.h264`，1,971,138 字节，540 个访问单元。
另用 FFmpeg 生成 180 帧、1080p30 的测试图，23,841,223 字节（约 31.8 Mbps）：

```bash
ffmpeg -hide_banner -loglevel error -f lavfi \
  -i 'testsrc2=size=1920x1080:rate=30' -frames:v 180 \
  -c:v libx264 -preset ultrafast -tune zerolatency -crf 8 -pix_fmt yuv420p \
  -x264-params 'aud=1:keyint=30:repeat-headers=1' \
  -f h264 artifacts/h264/test-pattern-1080p.h264
```

| 机器 / 样本 | 硬解帧数 / fps | 软解帧数 / fps |
|---|---|---|
| x358h / UU | 540 / 289.0 | 539 / 426.1 |
| KVM / UU | 540 / 324.3 | 539 / 349.4 |
| x358h / 测试图 | 180 / 181.6 | 180 / 245.3 |
| KVM / 测试图 | 180 / 236.0 | 180 / 238.7 |

两台硬解均输出 `1920x1080 Nv12`，`decoder after` 保持 D3D11VA。
测试图硬/软首帧导出的 RGB **逐字节完全相同**（三通道 MAE=0），并已目视确认彩条/图案正常。

这些是一次运行的墙钟耗时，含线程通信、GPU staging 回读；测试图双方均导出首帧，
UU 样本仅硬解导出首帧。**不能据此宣称硬解更快或 CPU 占用更低**，CPU 占用没有测量。
当前回读成本抵消了硬件优势；后续性能方向是 NV12 纹理直接上屏。

本机记录（被 gitignore）：`artifacts/hw-validation/{x358h,kvm-win11}.log`、
`*-hw.ppm`、`*-pattern-{hw,sw}.ppm`、`*-pattern-hw.png`、`probe-negative.log`。

## 4. GUI 验证

x358h 使用新 GUI 连接 KVM-WIN11，在自定义 55 Mbps 配置下，截图 HUD 显示：

```text
1920x1080   HW d3d11va (MF)
d3d11va (Media Foundation hw: Intel(R) Arc(TM) B390 GPU)
src 30.0 fps   dec 30.2 fps   render 30 fps
bitrate 11.10 Mbps   decode 2.99 ms
packets 11205   lost 21   errors 0
```

截图：`artifacts/hw-validation/x358h-gui-hw.png`。请求上限不等于实际码率，取决于内容。

另外开了一次 `UU_DECODER=sw` 对照会话（同样 `UU_FORCE_JOIN=0`）：截图
`x358h-gui-sw.png` 显示软解 `decode 14.58 ms`、窗口解码率 `14.9 fps`、
`lost 20, errors 104`。关闭时日志汇总 `samples=534 frames=415 errors=118`。
这次连接存在丢包，软解报错原因尚未专项定位；不能把不同会话的 2.99/14.58 ms
当作严格同输入跑分，也不能把软解实况写成零错误。后续可结合弱网恢复、PLI 和
缺失的 NACK/RTX 处理继续分析。

SSH / WMI 直接启动 GUI 位于 session 0，只能得到黑色截图。**交互式 GUI 测试改用
`New-ScheduledTaskPrincipal -UserId yfy -LogonType Interactive` 创建临时任务**，
完成测试后删除；不要把 WMI 的 session 0 黑图当成渲染故障。
两次交互式验证均已结束，临时任务 `uu-hw-validation` 已删除，无遗留测试 GUI 进程。

## 5. 验证与构建

```text
cargo test --workspace                  # 66 passed
cargo test -p uu-media --features vaapi # 32 passed（含纯 CPU 布局测试，非 VA-API 真机测试）
cargo build --release -p uu-gui -p uu-cli --target x86_64-pc-windows-gnu
```

`dist/uu-gui-win64/` 已更新 GUI、补齐 CLI，并重写 README 为真实硬解状态。
测试机 `C:\Users\yfy\uu-hw-probe\` 中保存两个 exe、依赖 DLL、样本和导出图像。
x358h 日常演示目录 `C:\Users\yfy\uu-gui-demo\` 也已同步两个 exe 与 README。
远端 `Get-FileHash` 与本机 SHA-256 一致：

| 文件 | SHA-256 |
|---|---|
| `uu.exe` | `57fcd7733f88ed4b3fbe5e3eff599e1e1051a0e2a95a74fc96221556ed5ff16b` |
| `uu-gui.exe` | `8a98ffd0edfee859121cf9b249f8885b3d38332c44a322effb8c55b698767397` |

## 6. 后续

1. Linux VA-API 真机验证仍待有可访问的 Intel/AMD Linux GPU。
2. D3D11VA 已通过 Intel/AMD 的同步 MF 解码器路径；异步 MFT 暂未实现。
3. 性能优化应优先评估 NV12 直接呈现，减少 CPU 回读、RGBA 转换和纹理再次上传。
4. x358h 当前高 DPI 下窗口仍可能超出屏幕；这是已有窗口尺寸问题，可后续单独调整。
5. 原有改动与本轮改动仍未提交；保留已有 handoff 迁移和码率等修改。
6. 软解实时对照会话出现丢包与解码错误，离线样本则正常；需要单独复现和定位。
