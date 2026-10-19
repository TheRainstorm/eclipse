# 10 · 窗口缩放、Moonlight HUD、菜单确认与退出/恢复修复

日期：2026-10-19。用户要求每完成一项功能就提交，并继续自主打磨至北京时间 13:30。
测试仍只连接 KVM-WIN11（`aeawvmepembcpca5`），`UU_FORCE_JOIN=0`；未连接 FYYUAN。
x358h 为 `192.168.34.3:2202`，KVM 为 `192.168.35.6:2202`，均 SSH 用户 yfy。

## 用户反馈与根因

1. **非全屏不能缩放**：无系统装饰的窗口没有实现边缘 resize。新增四边/四角原生 BeginResize、方向光标；边缘操作不向远端转发。
2. **HUD**：旧 RTT 从未赋值；旧“显示 FPS”只计纹理更新，停止出帧后还保留旧数值。改为顶部紧凑性能条，分辨率@实际新图呈现率、真实 RTP codec、Rx/De/Rd、Network 均值±标准差、Loss%、Bandwidth、Render/Decode/Encode。
3. **菜单确认**：选择画质后未关闭子菜单；顶部控制中心还会被同一 opening click 当成外部点击立即关掉。修复点击范围，确认后保留主菜单；质量子菜单现在向侧面展开，优先向左，不遮住主菜单。
4. **恢复停顿**：每次传输丢失都重建硬解，后续丢失会废掉完整排队 IDR。现在普通丢失保留解码器，完整旧 IDR 可独立解码呈现，但不放行依赖坏链的 P 帧。仍严格丢弃残缺 AU，默认不以花屏换流畅。
5. **退出状态残留**：GUI 立即丢句柄，后台线程来不及 release；连接途中不响应取消，异常 `?` 跳过 cleanup。改为统一、限时 finalizer，连接各阶段可取消，GUI 等 stopped 再退出/允许重连，收到主机 released 也结束连接。renderer shutdown 有兜底清理。

后续打磨还修复：

- 恢复 bool 存在检查后写入竞争：旧 IDR 完成会覆盖新 fault 的 waiting=true。改为 requested/completed 代次比较，迟到完成不会清除新请求。
- 重排器缺包期间每收到一包扫描两遍增长中的 HashMap，形成二次开销。缓存缺口期限，仅超时才查最近序号。
- 重传窗口随实测 RTT + 4×标准差 + 20ms 放宽，仍限定至 200ms；完整顺序帧没有额外等待。
- 解码器无输出回退原来只检测“从未出过帧”，出过一帧后永久无输出不再触发。现按连续空输出监测，30 AU 或 750ms 后回退；并不是对阻塞中的驱动调用做硬中断。
- 初始设置仅在 CONTROL 发送失败时重试，发送成功后不再无条件重发 5 次；GUI 不再提前排一份重复设置，减少启动配置扰动。
- 解码后直接 request_repaint，不再等待 GUI 16ms 轮询；保持每屏单帧 mailbox，移除从未使用的 RGBA 副本。
- view_only 不再阻断已经按下的键/鼠标释放消息。PeerConnection/DataChannel 回调改 Weak，避免自引用保留会话。

## HUD 的测量边界

- **Codec** 来自实际 video track MIME。当前只协商 H264，未实现 HEVC/AV1 解码；不是根据主机能力列表假称已使用某编码。
- **Network** 用现有 CONTROL SimpleAction echo，按 seq 匹配一次响应，保留最近 32 次，显示均值±标准差。webrtc-rs 0.11 的 ICE candidate-pair RTT 实现只返回默认零，不能用它伪造 0ms。超过 5 秒无有效回包显示未知。
- **Loss** 是最近约 2 秒超时未修复的 RTP 序号比例，分母为按序接收的唯一包+丢包，重复 RTX 不放大分母。
- **Rd** 是新视频图实际提交至 UI 画布的频率，不是每次 UI repaint，也不是监视器实际扫描输出计数。
- **Render** 使用 eframe CPU frame time（UI 准备+渲染提交，不含 VSync 等待），并非 GPU timestamp / 到屏延迟。
- **Decode** 是解码调用时间，不包含 NV12→RGBA。**Encode N/A**：没有可验证的 UU 主机编码耗时。悬停 HUD 可看定义和实际硬解/回退后端。
- 画质会显示“已发送”；有匹配 request_id 的 CaptureSettingResponse(22) 才显示“远端已确认”。此主机不回确认时保留“已发送”，不根据点按钮就谎报生效。单元测试覆盖成功、拒绝、无关 RPC。

## 验证

- 工作区自动测试：85 项通过（详细日志在私有 artifacts 下）。
- 真实 Windows x358h：新 HUD 已截图；鼠标右下角拖动由 2200×1520 改为 2020×1400；WM_CLOSE 后 10 秒以内正常退出，日志确认 CONTROL/peer closed。
- 最新侧边菜单实机截图验证：点击“快速1M”后子菜单收起，主菜单保持显示，“已发送：快速1M”可见；实际流切换至1280×720（官方主机随档位缩放），证明选择确实发送而非只改本地文字。
- 无人工丢包的35秒噪点压力：20.7Mbps、末段解码30.1fps，lost=9、bad=16、errors=0、queue_drop=3、最长画面间隔903ms；该轮仍包含旧版初始设置重复发送。不能宣称正常网络零停顿。
- 减少重复初始设置后的最终35秒无注入噪点压力：实际1920×1080，末段14.98Mbps、Rx29.91/De30.54fps；lost=5、bad=10、errors=0、queue_drop=6、decoded=888，最长间隔1356ms，最终waiting=0并完成cleanup。此结果没有证明最长卡顿继续改善，仍未达到官方体验。
- 连续重新连接均 force_join=0；会话结束记录 `session cleanup completed`。
- `stream_check` 新增最长已呈现帧间隔及 RTT/丢包注入聚合计数；退出等待真实 stopped，超时失败，不用固定 sleep 假定清理完成。
- `UU_TEST_VIDEO_LOSS_MS=120` 仅在本接收端每 10 秒丢 120ms 包，并在该轮 1 秒内丢掉对应 RTX，刻意强制 IDR 恢复；默认关闭，不改系统网络/主机配置。测试只有 45 秒，不是长稳保证。
- 首次低变化桌面强制丢失：最长帧间隔 276–366ms；噪点高变化突然开始后，同一早期版本出现 3810ms 间隔，不能当成已彻底修好。
- 重排/自适应窗口后第一轮全程噪点+4 次故障：17.8–21.8Mbps 附近，最长间隔 1312ms、解码错误 0、最终恢复，仍有初始化队列丢弃 12。不同内容/时段，不是严格 A/B。
- 最终恢复代次/无输出监测版本，同样 45 秒、4 次 120ms 强制丢包：实际1920×1080，约23.4–29.2Mbps，最长间隔1288ms，decoder_errors=0，最终waiting=0、decoded=1037、queue_drop=11；注入4174包（含被刻意屏蔽的RTX），lost=1444。不能把强制丢掉重传的场景当作正常网络体验。

`cargo fmt --all -- --check` 与全工作区 `clippy -D warnings` 暴露既有格式及 lint 问题（例如 convert.rs identity_op、旧代码新版本 is_multiple_of lint）；没有为了清它们混入全仓格式/无关修改。实际构建/测试通过。

私有截图、宏、日志位于 `artifacts/menu-stream-validation/`，真实桌面截图不提交。
参考原项目保持只读。重点参考 N/VideoDepacketizer.c（保留完整 IDR、15 帧有界队列）以及 M/app/streaming/video/ffmpeg.cpp（Rx/De/Rd、RTT、阶段耗时定义），U/capture_setting.rs（21/22/25 字段语义）；未复制 GPL 实现代码。

## 后续重点

- 严格恢复仍要等待主机新 IDR；在大 IDR 连续丢失时不能承诺没有停顿。FEC 与 GameStream RFI 仍不能单方面套在 UU 主机上。
- 初始解码队列仍有溢出（当前8 AU）；可对比 Moonlight 的15 AU上限，配合可丢弃的显示队列量化启动突发。不要只加缓冲掩盖持续慢解码。
- 当前 CPU RGBA 转换、GPU 回读/上传仍存在；后续优先 NV12 GPU 呈现、准确阶段队列计时和 PTS frame pacing。
- 比较官方客户端时应固定同一动态内容、实际分辨率和 codec；随机噪点只是压力场景。
- 音频、HEVC/AV1、剪贴板等不在本轮完成范围。

## 分发包

Windows release GUI/CLI 已重建，`dist/uu-gui-win64/` 两个 exe 与 README 一并更新：

- `uu-gui.exe` SHA-256：`af206cfb0834d4ac51689c2473a8eec655b9cb74756ec8a65beb305bae62632d`
- `uu.exe` SHA-256：`87e1fa2be5031eb3b1255b72b29f7082405e8fd2a68ba1d7070151f557514845`

x358h 安装位置：`C:\Users\yfy\uu-gui-demo\`、`rtx-auto\`、`hud-recovery\`。
原有 GUI 进程 PID41384 保留；运行中 exe 先改名备份再写入新版，不结束用户进程。
重新启动 GUI 才会运行新版本。
