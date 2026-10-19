# 串流恢复、卡顿与 FEC

更新：2026-10-19。对应本轮输入与接收链路改动，最新实测见 `handoff/10-2026-10-19-resize-hud-recovery-exit.md`；前一轮见交接 07。

## 现有问题为什么会发生

H.264 的一帧通常拆成几十到上千个 RTP 包。原接收器按到达顺序拼接 FU-A，
没有验证序号连续性，在时间戳变化时还会把未完成的访问单元送去解码。
一个丢失/乱序的分片可能破坏整个 NAL；旧 depacketizer 的 FU 缓冲还可能跨帧残留。
这会造成解码报错、局部花屏，或参考帧损坏后持续异常到下一张 IDR。

卡顿还可能来自解码/颜色转换挤占异步收包线程、显示队列积压、丢包后的恢复等待，
以及源帧率与显示刷新节奏不匹配。当前 `decode_ms` 只计解码调用，不包含 RGBA 转换、
UI 上传和最终显示，因此不能把它当成端到端延迟。

## 本轮具体改动

- RTP 按序号重排，支持 16 位回绕、去重和超时；只在缺包时等待，完整的顺序帧立即输出。
- 默认缺包窗口 60 ms，`UU_JITTER_MS=10..200` 可调；RTT/波动较高时自适应放宽，最多 200ms。完整帧不增加等待。
- FU-A 必须有完整首尾且连续，STAP-A 校验所有长度；缺 marker、截断、错误拼接的帧丢弃。
- webrtc-rs 原本已经注册 NACK，旧交接“没有 NACK”不准确。其默认间隔 100 ms，
  当前改为由应用重排窗口每 20 ms 生成 NACK；已补齐 RTX 接收，恢复后停止请求同一缺包。
- 丢失/坏帧触发单个 PLI，通知反馈任务立即检查，前两次请求间隔至少 200ms，之后每 500ms 限速重试，连续失败再升级 FIR；移除伪造发送者 SSRC 的重复 PLI。健康串流不强制关键帧。
- 最新默认严格恢复：丢失参考链后等待新 IDR，网络丢失保留健康解码器，已经完整排队的旧 IDR 仍可呈现；并在解码出真实画面、确认请求代次和 PTS 后结束恢复。解码器重建保留 RTP 派生时间戳，避免 Windows 合成 60 Hz 时钟从零开始导致恢复确认失败。
- `UU_STRICT_RECOVERY=0` 可恢复此前的平衡模式（继续解完整依赖帧），但它可能短暂显示预测错误，不再作为默认。
- SPS/PPS 本身不再被当作关键帧；参考链恢复必须识别真实 type-5 IDR。
- 解码/颜色转换移入阻塞工作线程，压缩帧队列有界（8 帧）。追赶突发时保留参考帧解码，
  跳过旧帧的 RGBA 转换和呈现。
- UI 每屏最多保留一张待显示图，减少长延迟、重复纹理上传和内存积压。
- SDP 只声明本程序实际接收的 H.264/Opus；不再沿用默认集合中没有恢复实现的 ULPFEC 声明。

恢复完成按 requested/completed 代次比较，避免旧 IDR 完成清除新丢失请求。重排器缓存缺口期限，避免每包扫描整张待重排表。GUI 在新帧到达时立即唤醒，不再依赖固定 16ms 轮询。详见交接10。

## Sunshine / Moonlight 的可借鉴技术

| 机制 | 解决什么 | 在本项目中的边界 |
|---|---|---|
| FEC（Reed–Solomon） | 用冗余包恢复一定范围的丢包，省去重传的 RTT | 需要发送端提供兼容校验数据；当前没有接收实现，不能单方面生成网易没发来的校验包 |
| 参考帧失效通知 / IDR 恢复 | 参考链损坏后快速恢复；并非所有情况都需要整张大关键帧 | Moonlight 有 GameStream 专用 RFI；网易 WebRTC 目前走 PLI/FIR，不能直接复用其控制协议 |
| GPU 解码与 GPU 呈现 | 避免 GPU→CPU→GPU 回读、颜色转换和再次上传 | 本项目已硬解，但 egui/glow 仍吃 CPU RGBA；直接 GPU 呈现需要后续后端改造 |
| Frame pacing / VSync / VRR | 降低显示间隔抖动、撕裂和不必要的等待 | 当前有 VSync，没有完整的帧节奏控制；官方被控端实测 30 fps 上限也会影响观感 |
| 对码率峰值和网络瓶颈的控制 | 减少平均带宽够用但突发帧仍把链路打爆的情况 | GUI 可调请求码率；真实包发送节奏主要由网易被控端决定 |

Sunshine 的 `fec_percentage` 当前文档默认是 20，含义是每份数据附加约 20% 校验包，
代价是带宽增加。它不是“任意丢掉链路 20% 的包都能恢复”的保证，恢复能力还取决于分块和
哪些数据/校验包丢失。Moonlight 源码按 FEC block 用 `reed_solomon_decode` 重建缺失包。

当前已接入 RFC 4588 SSRC-multiplexed RTX：从协商后的 SDP 解析 H.264 apt 和 FID，
在 webrtc-rs 0.11 repair reader 的 interceptor 边界接收重传，恢复原 PT/SSRC/序号，
交给同一有界重排器。没有修改第三方库，也没有把修复包误当新视频帧。
KVM-WIN11 已实际协商并返回 RTX；FEC（包括 RED/ULPFEC）仍未实现，不能混为同一项能力。
RTX 计数包括重复/探测包，不能把该计数直接解释成修复了多少次画面错误。

## 自动码率和能力声明

旧版初始化缺少 ConnectOptions 的 DecoderCap，streamer JSON 固定 1080p，GUI 又默认自定义55M。
最新补齐 H.264 420 的 3840×2160@60 解码能力、提供跟随远端/1080p/1440p/4K选择，
GUI 默认改用官方 `quality=5` 自动档。自动档由被控端结合 WebRTC 反馈调节，不是客户端固定发18M。
x358h→KVM-WIN11 的短测在实际1920×1080码流上达到17.4–19.0Mbps、约30fps，见交接09。
官方截图中的“4K auto”是档位信息，不能单凭它认定实际传输分辨率；比较要看解码后的实际尺寸。

## 下一步优先级

1. 做可重复的同码流丢包、乱序、突发测试，记录接收、解码、颜色转换、排队、呈现各阶段耗时。
2. 继续用可控丢包、乱序和长稳测试量化 RTX/NACK 的有效恢复；RED/ULPFEC 需要另行确认协商与实现。
3. 为 NV12 做直接 GPU 呈现，减少回读及 CPU RGBA 转换。
4. 做可配置帧节奏与网络自适应，结合 RTT、丢包、队列长度调整带宽，而非固定追求高码率。

参考（均为官方资料/源码）：

- [Sunshine FEC 配置](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html#fec_percentage)
- [Moonlight RTP/FEC 重建](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/RtpVideoQueue.c)
- [Moonlight 参考帧/IDR 恢复](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/VideoDepacketizer.c)
- [Moonlight D3D11VA 呈现与帧节奏](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/d3d11va.cpp)
- [Sunshine 对突发发送与丢包的说明](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)
