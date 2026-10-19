# 串流恢复、卡顿与 FEC

更新：2026-10-19。对应本轮输入与接收链路改动，实测细节见 `handoff/07-2026-10-19-experience.md`。

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
- 默认缺包窗口 60 ms，`UU_JITTER_MS=10..200` 可调；不是给每一帧增加 60 ms。
- FU-A 必须有完整首尾且连续，STAP-A 校验所有长度；缺 marker、截断、错误拼接的帧丢弃。
- webrtc-rs 原本已经注册 NACK，旧交接“没有 NACK”不准确。其默认间隔 100 ms，
  本轮改为 20 ms，与短重排窗口配合。实际是否能补回包仍取决于网易被控端的响应。
- 丢失/坏帧立即触发 PLI/FIR 恢复；每 500 ms 限速重试，健康串流不再每 2 秒强制关键帧。
- 默认采用平衡恢复：丢弃损坏的访问单元，但硬解继续解后续完整帧，同时请求 IDR。
  真正解码失败或压缩队列溢出时，暂停依赖帧直到新 IDR。
- `UU_STRICT_RECOVERY=1` 可让每次丢包后也暂停等待 IDR。实机压力测试证明这可能明显
  加重停顿，尤其关键帧也缺包时，所以没有设为默认。平衡模式仍可能出现暂时的参考帧误差。
- SPS/PPS 本身不再被当作关键帧；参考链恢复必须识别真实 type-5 IDR。
- 解码/颜色转换移入阻塞工作线程，压缩帧队列有界（8 帧）。追赶突发时保留参考帧解码，
  跳过旧帧的 RGBA 转换和呈现。
- UI 每屏最多保留一张待显示图，减少长延迟、重复纹理上传和内存积压。
- SDP 只声明本程序实际接收的 H.264/Opus；不再沿用默认集合中没有恢复实现的 ULPFEC 声明。

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

目前没有声称已经接入 FEC 或完整 RTX：当前 webrtc-rs 0.11 的 repair-stream 读取路径
没有把 RTX 数据交付给应用进行视频重组。需要先确认官方被控端能协商哪些修复格式，
再实现对应的解包/恢复，并用可控丢包验证有效性。

## 下一步优先级

1. 做可重复的同码流丢包、乱序、突发测试，记录接收、解码、颜色转换、排队、呈现各阶段耗时。
2. 记录官方 SDP 和修复包类型，验证 NACK 的实际恢复效果，再决定 RTX / RED / ULPFEC 路线。
3. 为 NV12 做直接 GPU 呈现，减少回读及 CPU RGBA 转换。
4. 做可配置帧节奏与网络自适应，结合 RTT、丢包、队列长度调整带宽，而非固定追求高码率。

参考（均为官方资料/源码）：

- [Sunshine FEC 配置](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html#fec_percentage)
- [Moonlight RTP/FEC 重建](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/RtpVideoQueue.c)
- [Moonlight 参考帧/IDR 恢复](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/VideoDepacketizer.c)
- [Moonlight D3D11VA 呈现与帧节奏](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/d3d11va.cpp)
- [Sunshine 对突发发送与丢包的说明](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)
