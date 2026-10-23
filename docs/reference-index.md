# 开源实现参考索引

本索引记录参考项目的源码入口、机制与接入边界。串流现状见 [串流质量说明](streaming-quality.md)，
帧率协商见 [帧率说明](frame-rate.md)，Linux 运行与验证见 [Linux 客户端说明](linux-client.md)。
DTLS 接收竞态的修复依据见 [补丁说明](../vendor/webrtc-dtls/UU-PATCH.md)。

## 1. 阅读范围与固定版本

本轮按连接、收包、纠错、参考链恢复、解码、呈现、输入、声音、工具和测试逐层阅读关键实现；大型仓库的外围组件只列扩展入口，不代表逐行审计了全部源码，也不代表运行验收了这些参考项目。三个原始目录均只读，未执行拉取、构建、安装或修改。

| 标识 | 本地目录 | 本次 HEAD | 定位 |
|---|---|---|---|
| S | `foundation-sunshine` | `c4b9a3f5f1bb604f081c37a2e6e6df42b7d47f9f`（2026-10-18） | Foundation 的 Sunshine 被控端增强版 |
| M | `moonlight/moonlight-qt` | `914a4d74acea09e8c5f1e283c83c676f73fa9c34`（2026-09-06） | Moonlight V+ 客户端，含定制输入、ABR、剪贴板、文件映射 |
| U | `uu-remote-linux` | `b7f9f2aa683afb40e9b1bc95db88acf50fd69f85`（2026-10-19，v0.2.11） | 同协议的 Linux 被控端与实验性控制端；优先参考协议与工具 |
| D | `moonlight/deskflow` | `a930c87ea4ec5dc2e496424d58e365771eb44546`（2026-09-04） | 键鼠共享及各平台剪贴板；不是视频传输实现 |
| N | M 固定的 `moonlight-common-c/moonlight-common-c` 子模块 | `8a5d9562f2d557f4acf6e2f722e86502a68e5dd7` | 真正的 Moonlight 网络、RS FEC、参考链恢复层 |

以上原始工作区在阅读时均干净。M 的 common-c 子模块未初始化，不能据空目录判断“没有 FEC”。本轮在 `/tmp/uu-reference-moonlight-common-c` 独立取得并检出了 M 所固定的提交；下文 N 链接使用该提交的永久源码链接。S 比 M 新，不能假定二者的最新扩展均能在这两个快照间配套，例如 S 的 PyroWave 不应据此宣称当前 M 已支持。

### 接入分类

- **客户端适配**：状态机、队列、渲染、观测等思路可在本客户端实现；仍需用 Rust/现有后端适配。
- **UU 协议适配**：U 提供同协议证据，但要对目标 Windows/Mac 主机重新验证字段与能力。
- **两端配合**：Sunshine 专用协议、校验包、编码器/采集能力需要发送端；不能只改 UU 客户端就获得。

仓库许可也应随代码来源记录：本项目 workspace 声明 `MIT OR Apache-2.0`；S/M/N 的 LICENSE 为 GPLv3，U 为 `GPL-3.0-or-later`，D 为 GPLv2（部分文件带 OpenSSL exception）。本文记录结构和技术思路，未导入其实现代码。以后若整段复制、链接这些实现，应先确定采用方式和许可安排，不能把原 GPL 代码直接标成本项目的宽松许可。

## 2. 从体验问题快速找代码

| 现象/目标 | 先读 | 本项目落点 | 结论 |
|---|---|---|---|
| 中键无效、按住拖动失效 | M03、U06；下文“已知差异” | `uu-gui/input.rs` | 原值 3 错；当前 Windows 路径实测中键 16，同时必须发真实 down/up |
| 花屏、参考帧持续损坏 | S01–S03、N01–N02、U02 | `uu-core/video_receiver.rs`、`media.rs` | 分片完整性、重排、恢复状态机先于“提高码率” |
| 周期停顿、丢包后卡死 | N02、M01、U02–U03 | 解码队列、IDR 请求、显示 mailbox | 解码参考帧与丢弃旧显示帧要分层处理 |
| 硬解仍占 CPU、延迟高 | M02、S05 | `uu-media/hw/`、`uu-gui` 渲染后端 | 0.9已用 GPU 颜色转换及平面上传；硬解表面仍同步读回 CPU，尚非全 GPU 呈现 |
| 平均 FPS 足够却不顺滑 | M01、M07、S08 | UI 重绘、呈现调度、分阶段统计 | VSync 开关不能替代 frame pacing；要测帧间隔分位数 |
| 想用 FEC | S01、N01、S09 | 先抓 SDP、NACK 与实际修复包 | RS、WebRTC ULPFEC、PyroWave XOR 是不同线上格式 |
| 声音、剪贴板、文件、终端 | M05–M06、U07–U11 | 新媒体/工具模块与 capability handshake | U 更接近 UU；不能照搬 Foundation 私有控制包 |
| 外观、全屏、悬浮菜单 | M08、U13 | `uu-gui/ui.rs` | 主界面参考用户提供的 UU 图；Moonlight 参考交互和功能组织 |

## 3. Foundation Sunshine（S）

| 编号 / 能力 | 已阅读入口 | 实现要点与接入判断 |
|---|---|---|
| S01 RS FEC 分块、自动保护 | plan_block / plan / controller_t（`foundation-sunshine/src/streaming/fec.cpp:57`）；RS 编码与发送（`foundation-sunshine/src/stream.cpp:1258`）；分块与反馈测试（`foundation-sunshine/tests/unit/test_fec.cpp:1`） | 每块数据+校验不超过 255 shards，最多 4 块；10 秒反馈窗口，反馈过期 3 秒回退，缺反馈不等于无丢包。自动比例用独立丢包模型估计，并明确不是突发丢包保证。**两端配合**。 |
| S02 FEC 与总带宽预算 | bitrate_budget_t（`foundation-sunshine/src/streaming/bitrate.cpp:17`）；视频预算接线（`foundation-sunshine/src/video.cpp:231`） | 编码预算约为视频总预算×100/(100+FEC%)，还扣音频/开销；1 秒窗口抑制重配置抖动，编码器接受后才 commit 预算。可借鉴带宽分层，不能把编码码率当总线上码率。 |
| S03 参考帧失效 / IDR | IDR 控制处理（`foundation-sunshine/src/stream.cpp:1906`）；编码器失效接口（`foundation-sunshine/src/video.cpp:672`）；失效事件处理（`foundation-sunshine/src/video.cpp:3679`） | 根据编码器能力选择 RFI，失败/不支持时用 IDR；减少不必要的大关键帧。**两端配合**，GameStream RFI 消息不能替代 UU PLI/FIR。 |
| S04 发送节拍和批量发送 | 帧内 pacing / send_batch（`foundation-sunshine/src/stream.cpp:3562`） | 批量限制小于约 64 KiB、最多 64 包，跨帧保留时间债务，避免尾部 burst。该快照采用约 80% 千兆速率的组节流，不能误称为自动匹配当前 Wi-Fi 容量。**发送端改造**才生效。 |
| S05 低延迟编码与采集流水线 | 禁 B 帧 / VBV（`foundation-sunshine/src/video.cpp:2770`）；异步采集（`foundation-sunshine/src/video.cpp:1183`）；GPU 采集（`foundation-sunshine/src/platform/windows/display_vram.cpp:1`）；NVENC（`foundation-sunshine/src/nvenc/common_impl/nvenc_base.cpp:1`）；AMF（`foundation-sunshine/src/amf/amf_d3d11.cpp:1`） | 软件 zerolatency、硬件 async_depth、VBV、捕获/编码解耦都影响真实延迟。适合未来自有 host，UU 官方主机上客户端无法任意修改这些参数。 |
| S06 可解释的 ABR | fallback_decision / process_feedback（`foundation-sunshine/src/streaming/abr.cpp:499`）；ABR API（`foundation-sunshine/src/nvhttp/abr_api.cpp:1`） | 丢包>5%降到70%，>2%降到90%，<0.5%稳定后探升5%；普通决策间隔3秒，严重丢包走立即降档。LLM 目标是额外层，不是低延迟闭环必需。可先实现纯客户端规则，通过已验证的 UU 自定义码率请求执行。 |
| S07 本地光标与最新值桥接 | cursor_channel（`foundation-sunshine/src/cursor_channel.h:11`）；光标采集（`foundation-sunshine/src/platform/windows/display_cursor.cpp:1`） | 独立同步形状、可见性、hotspot、revision，客户端本地画光标可摆脱视频帧率限制。客户端机制可适配，但线上消息要走 UU 光标协议。 |
| S08 分阶段性能统计 | pipeline_sample_t / subscriptions（`foundation-sunshine/src/streaming/perf_recorder.h:31`）；frame-rate tracker（`foundation-sunshine/src/streaming/frame_rate_tracker.h:1`）；统计测试（`foundation-sunshine/tests/unit/test_perf_frame_rates.cpp:1`） | 区分 capture/convert/encode_queue/encode/packet 阶段，区分重复帧与新帧，按需采集。客户端可用同样方法拆 receive/decode/convert/upload/present，不冒充主机端耗时。 |
| S09 PyroWave / GPU 互操作 | 集成边界（`foundation-sunshine/src/pyrowave/README.md:1`）；packetizer（`foundation-sunshine/src/pyrowave/packetizer.cpp:1`）；GPU 同步合同（`foundation-sunshine/src/pyrowave/gpu_interop.h:1`） | 独立编解码/封包路径，PYRF envelope，最多16数据块加1 XOR parity，deadline 后丢不可恢复帧，传统 RS parity 禁用。需能力协商、双方实现及 Vulkan 互操作；不是给 H.264 开一个开关。列为远期研究。 |
| S10 虚拟屏 / 直接捕获 | ZakoVDD direct capture（`foundation-sunshine/src/platform/windows/display_vdd.cpp:1`）；frame channel（`foundation-sunshine/src/platform/windows/vdd_frame_channel.h:1`） | 从虚拟显示驱动共享 D3D11 纹理，绕过 Desktop Duplication/WGC；仅对应虚拟屏。未来自有 host/虚拟桌面参考，不能改变现有官方 UU host。 |
| S11 HDR 与图像增强 | HDR 选择（`foundation-sunshine/src/hdr/dynamic_hdr_selection.cpp:1`）；码流 metadata（`foundation-sunshine/src/video_hdr_bitstream.cpp:1`）；增强边界（`foundation-sunshine/src/image_enhancement/README.md:1`） | 采集像素、位深、颜色空间、metadata、显示能力要一致。HDR/444 不能只靠 SDP 声明；本项目 H.264 420 稳定后再扩展。 |
| S12 文件/剪贴板/麦克风/USB | 文件映射 RPC（`foundation-sunshine/src/file_mapping/file_mapping_rpc.cpp:1`）；大剪贴板 blob（`foundation-sunshine/src/clipboard_blob_store.h:1`）；麦克风混音（`foundation-sunshine/src/mic_mixer.h:1`）；USB/IP 生命周期（`foundation-sunshine/src/remote_usb/remote_usb_host_controller.h:1`） | 作为架构扩展入口：独立服务、缓冲边界、会话回收值得参考；这些 Foundation 扩展都需要 host 支持。UU 工具优先参照 U。 |

### FEC 在这里究竟能做什么

S 的配置默认 `fec_percentage=20`（配置默认值（`foundation-sunshine/src/config.cpp:533`）），表示相对数据附加约20%的校验。例如理想单块100数据+20校验，在约束内可恢复任意20个 shard 的缺失；不是任意20%总网络包损失均可修复，更不能保证任意突发长度。自动保护还必须给校验预留带宽，否则增加 FEC 本身可能加重拥塞。

UU 当前没有可用的 RS/ULPFEC 接收实现，也没有确认官方主机在当前协商中提供兼容校验包。正确顺序是确认 SDP 与实际 RTP，再按对应格式恢复；不发送虚假的支持能力。NACK、RTX 和 FEC 必须分别验证，不能把“注册 NACK interceptor”计为“重传已成功恢复”。

## 4. Moonlight V+ 与 common-c（M/N）

| 编号 / 能力 | 已阅读入口 | 实现要点与接入判断 |
|---|---|---|
| N01 视频 RS 恢复与包重排 | [reconstructFrame / RtpvAddPacket](https://github.com/qiin2333/moonlight-common-c/blob/8a5d9562f2d557f4acf6e2f722e86502a68e5dd7/src/RtpVideoQueue.c#L194) | 先重建完整 FEC block，验证恢复包，组完整帧再交 depacketizer；恢复包不算原始乱序观测。学习分层、统计与有效性检查；NV_VIDEO_PACKET/fecInfo 是 GameStream 专用，不能传入 UU RTP depacketizer。 |
| N02 参考链恢复和队列溢出 | [dropFrameState](https://github.com/qiin2333/moonlight-common-c/blob/8a5d9562f2d557f4acf6e2f722e86502a68e5dd7/src/VideoDepacketizer.c#L99)；[队列溢出处理](https://github.com/qiin2333/moonlight-common-c/blob/8a5d9562f2d557f4acf6e2f722e86502a68e5dd7/src/VideoDepacketizer.c#L560)；[notifyFrameLost](https://github.com/qiin2333/moonlight-common-c/blob/8a5d9562f2d557f4acf6e2f722e86502a68e5dd7/src/VideoDepacketizer.c#L1182) | IDR/RFI 等待状态分离；解码器成功处理 IDR 后才确认恢复，压缩队列溢出清队列并请求 IDR。与本项目 request/decoder 两种 epoch 设计对照，防止旧 IDR 清除新故障。 |
| N03 音频纠错 | [音频 RS queue](https://github.com/qiin2333/moonlight-common-c/blob/8a5d9562f2d557f4acf6e2f722e86502a68e5dd7/src/RtpAudioQueue.c#L52)；[接收与播放线程](https://github.com/qiin2333/moonlight-common-c/blob/8a5d9562f2d557f4acf6e2f722e86502a68e5dd7/src/AudioStream.c#L1) | 音频也有单独 RTP/FEC 队列；不同于视频 RS、也不同于 Opus in-band FEC。UU 音频优先实现自己的 Opus jitter/PLC，不混用格式。 |
| M01 帧节奏与 VSync | handleVsync / submitFrame（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/pacer/pacer.cpp:201`）；队列合同（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/pacer/pacer.h:14`）；DX VSync（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/pacer/dxvsyncsource.cpp:1`） | pacing/render 队列有界，持有队列最多3帧，加正在渲染/延迟释放最多5帧；500ms历史决定丢旧显示帧力度。可适配客户端，但不能照搬常量而忽略30/60/120Hz与帧到达时间。 |
| M02 硬解→GPU呈现 | 低延迟解码与测试帧（`moonlight/moonlight-qt/app/streaming/video/ffmpeg.cpp:527`）；共享设备 / fence（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/d3d11va.cpp:213`）；纹理 bind/copy（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/d3d11va.cpp:1151`） | AVFrame硬件纹理直接GPU颜色转换/呈现，按GPU选择bind/copy，共享设备有双向同步；开流前实际解码/渲染测试帧。不是永远无GPU拷贝，但能避免逐帧CPU回读。是本项目硬解后最值得适配的性能项。 |
| M03 物理键鼠事件与移动合并 | handleMouseButtonEvent（`moonlight/moonlight-qt/app/streaming/input/mouse.cpp:16`）；移动合并（`moonlight/moonlight-qt/app/streaming/input/mouse.cpp:114`）；捕获/焦点状态（`moonlight/moonlight-qt/app/streaming/input/input.cpp:1`） | down/up独立；画面外拒绝新按下、允许抬起；过滤触控/笔合成的重复鼠标事件；合并连续motion。可参考，但 LiSendMouseButtonEvent 的编号属于另一协议。保持 button、wheel、key 边界顺序，不能跨过它们合并移动。 |
| M04 桌面/游戏鼠标与快捷键 | 特殊组合键（`moonlight/moonlight-qt/app/streaming/input/keyboard.cpp:76`）；本地光标状态（`moonlight/moonlight-qt/app/streaming/input/input.cpp:460`）；系统光标分类（`moonlight/moonlight-qt/app/streaming/input/cursorshapeclassifier.cpp:1`） | 绝对桌面与相对捕获模式分开；本地快捷键不穿透；根据远端shape/visibility渲染光标。可适配，UU还需对应消息与相对移动协议证据。 |
| M05 声音缓冲与播放反馈 | submitAudio / prepareForPlayback（`moonlight/moonlight-qt/app/streaming/audio/renderers/sdlaud.cpp:96`）；解码和音频能力（`moonlight/moonlight-qt/app/streaming/audio/audio.cpp:1`） | 音频设备缓冲至少10ms，按帧长取3帧；协议队列>30ms时跳旧音频，SDL队列有50ms阈值，设备失效触发重建。学习有界延迟与设备恢复，具体阈值需在UU音轨测试。 |
| M06 剪贴板与文件映射 | hash+TTL 去回环（`moonlight/moonlight-qt/app/streaming/clipboardsync.h:43`）；挂载会话入口（`moonlight/moonlight-qt/app/streaming/filemappingclient.cpp:1`）；协议客户端（`moonlight/moonlight-qt/file-mapping/protocol/file_mapping_client.cpp:1`） | 剪贴板helper隔离GUI线程；文本/PNG，hash+时间防回环，大payload转blob；文件映射抽象跨平台mount。客户端机制可借鉴，wire和blob API需换UU实现。 |
| M07 HUD与ABR反馈语义 | Rx/Decode/Render 统计（`moonlight/moonlight-qt/app/streaming/video/ffmpeg.cpp:921`）；sendSunshineAbrFeedback（`moonlight/moonlight-qt/app/streaming/session.cpp:3356`）；带宽采样（`moonlight/moonlight-qt/app/streaming/network/bandwidth.cpp:97`） | 分开计解码、排队和渲染时间；ABR每3秒发送窗口增量，防止请求重叠。此快照loss由FEC失败/OOS/invalid构造，反馈参数用m_ActiveVideoFrameRate；不能当实测解码FPS。带宽优先读系统网络统计，也不能直接当单会话吞吐。 |
| M08 悬浮操作与全屏 | 菜单面板（`moonlight/moonlight-qt/app/streaming/video/overlaymenupanel.cpp:1`）；overlay 生命周期（`moonlight/moonlight-qt/app/streaming/video/overlaymanager.cpp:1`）；Linux事件监视（`moonlight/moonlight-qt/app/streaming/video/overlayeventmonitor_linux.cpp:1`） | 控件只呈现当前可用能力；悬浮菜单不能吞掉/泄露游戏输入，退出时回收timer/hook。学习交互结构；不需要把当前egui整体替换成Qt。 |
| M09 其它硬件后端 | VAAPI（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/vaapi.cpp:1`）；EGL呈现（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/eglvid.cpp:1`）；VideoToolbox/Metal（`moonlight/moonlight-qt/app/streaming/video/ffmpeg-renderers/vt_metal.mm:1`） | Linux dma-buf/EGL与Mac Metal方向可作未来GPU接口设计参考；原生资源生命周期、色域与同步仍需分别实现。 |
| D01 跨平台剪贴板/键鼠 | Client::enter / leave（`moonlight/deskflow/src/lib/client/Client.cpp:232`）；Portal clipboard（`moonlight/deskflow/src/lib/platform/PortalClipboard.h:19`）；Windows clipboard（`moonlight/deskflow/src/lib/platform/MSWindowsClipboard.cpp:1`） | 多种MIME、owner与变更状态、异步Portal读写超时；用于以后跨平台桌面协作。Deskflow没有视频链路，不作为FEC或解码参考。 |

0.9的 egui/glow 已能消费 CPU NV12/I420 平面并用着色器转换颜色，但仍不能直接接收 D3D11 解码表面。完整实施 M02 前需要一个 GPU frame/surface 合同（资源所有权、设备/适配器、可见尺寸/stride、PTS、颜色空间、fence、回退路径），以及能消费它的 Windows renderer；只把NV12转换函数改成shader仍未解决硬解回读。

## 5. uu-remote-linux（U）：同协议优先参考

| 编号 / 能力 | 已阅读入口 | 实现要点与本项目接入位置 |
|---|---|---|
| U01 信令和会话路由 | Session::open / step / close（`uu-remote-linux/src/viewer/session.rs:10`）；Engine.IO / Socket.IO（`uu-remote-linux/src/viewer/signal.rs:1`）；JoinData / retry policy（`uu-remote-linux/src/viewer/api.rs:57`） | 云端ack可能重新分配ice_id；候选与早到消息缓冲；released事件仅关闭匹配路由；错误要回收peer。对照本项目controller/socketio，工具多peer时尤其重要。 |
| U02 H264/H265收包 | Receiver::push / expire / hevc（`uu-remote-linux/src/viewer/media.rs:19`） | H264 Single/STAP/FU，H265 Single/AP/FU，无DONL；AU上限8MiB，32包重排，40ms缺口/200ms残尾定时丢弃，有明确损坏标记。可借鉴HEVC与测试；高码率大IDR不应盲目换成32包窗口。 |
| U03 RTC调度和恢复 | Peer::new / on_track（`uu-remote-linux/src/viewer/rtc.rs:105`）；display / Decoder / FrameSink（`uu-remote-linux/src/viewer/desktop.rs:13`） | on_track立刻spawn，否则audio读循环会阻止video回调；明确mime；PLI限250ms；解码错误flush并请求刷新，AU队列容量3。可对照多轨/错误恢复；丢压缩参考帧后仍需更严格的恢复门控。 |
| U04 解码与帧率保真 | FFmpeg库解码（`uu-remote-linux/src/viewer/decoder.rs:5`）；CLI passthrough探测（`uu-remote-linux/src/viewer/decoder_cli.rs:52`） | H264/H265格式切换、corrupt帧拒绝、stride逐行拷贝；CLI必须fps passthrough，避免无时间戳裸流被丢帧。CLI RGB输出/逐帧缩放不是Moonlight式GPU呈现方案。 |
| U05 协商/设置/色度 | Shape / decoder caps（`uu-remote-linux/src/viewer/wire.rs:25`）；设置解析与同步（`uu-remote-linux/src/capture_setting.rs:124`）；同轨codec选择（`uu-remote-linux/src/video_track.rs:23`） | 显式能力交集，H265由host选；真彩field7、codec/chroma同步消息，必须确认host实际应用。当前本项目H264420优先，不能提前声明HEVC/444/HDR。码率细节仍以Windows抓包为准。 |
| U06 输入状态与焦点清理 | Input / release_all（`uu-remote-linux/src/viewer/input.rs:52`）；SDL事件翻译（`uu-remote-linux/src/viewer/sdl.rs:64`）；host会话所有权（`uu-remote-linux/src/input.rs:475`）；VK映射（`uu-remote-linux/src/input_mapping.rs:1`） | 同协议状态、只读开关、失焦松键、旧session迟到事件隔离值得参考。**此快照viewer中键值4不适用于本轮Windows目标**；不据Mac/Linux规则覆盖实测16。键码也按目标平台区分。 |
| U07 声音 | Opus队列与Playback（`uu-remote-linux/src/viewer/audio.rs:8`）；host Pulse/PipeWire→Opus（`uu-remote-linux/src/audio.rs:1`） | viewer有12包压缩队列、Ogg封装喂FFmpeg、PCM队列与SDL积压清理。可快速补声音，但进程/缓冲开销较大；长期方案用进程内Opus+时钟+jitter/PLC。useinbandfec=1声明不等于已实现Opus FEC恢复。 |
| U08 剪贴板 | Bridge / Backend（`uu-remote-linux/src/viewer/clipboard.rs:158`）；host桥接（`uu-remote-linux/src/clipboard.rs:1`） | 同协议feature协商、目标Mac/Windows差异、文本边界与异步I/O；适合在controller新增bridge和平台后端。本项目旧协议框架不等于GUI已能双向同步。 |
| U09 文件传输 | Files / inflate / upload / download（`uu-remote-linux/src/viewer/tools/files.rs:11`）；Transport / buffered_amount（`uu-remote-linux/src/viewer/tools/mod.rs:20`） | RPC走CONTROL、数据/响应走FILE；zlib目录、任务ID、取消和超时、分片、临时目录和路径校验、发送背压。应独立capture_type=5会话，不能用Foundation文件映射API替代。 |
| U10 终端 | TERM帧 / Terminal（`uu-remote-linux/src/viewer/tools/terminal.rs:10`）；本地终端IPC（`uu-remote-linux/src/viewer/tools/terminal_ipc.rs:1`） | capture_type=8，BINARY流，TERM帧长/会话ID/resize/open/attach/exit；终端输入输出不能记录进日志。独立工具会话，关闭桌面不连带关闭。 |
| U11 端口映射 | PortMap / frame / forward（`uu-remote-linux/src/viewer/tools/portmap.rs:10`）；工具协议测试（`uu-remote-linux/src/viewer/tools/tests.rs:1`） | capture_type=9，Message27，多rule/stream、SYN/ACK/FIN、断开清理；现有实现不支持TCP half-close。GUI做独立工具窗口前先验证路由与背压。 |
| U12 光标与RTP附加信息 | CursorImage / Message15（`uu-remote-linux/src/cursor.rs:103`）；CaptureIndex / NewFrame（`uu-remote-linux/src/rtp_metadata.rs:26`） | UU形状消息在SystemStateChange field2；capture-index/new-frame是扩展元数据。先从host端编码反推解析，再用官方Windows包确认。不能把capture index直接解释成可测主机墙钟延迟。 |
| U13 设备工具和远程协助 | 启动器（`uu-remote-linux/src/viewer/ui.rs:1`）；工具窗口（`uu-remote-linux/src/viewer/tools_ui.rs:1`）；协助码stdin传递（`uu-remote-linux/src/viewer/assist.rs:1`）；系统动作（`uu-remote-linux/src/viewer/power.rs:1`）；设备解绑API（`uu-remote-linux/src/viewer/api.rs:158`） | 学习状态、能力和工具会话隔离；协助/解绑/电源属于独立功能，不能混入串流自动测试。索引不表示已经在本项目启用这些操作。 |
| U14 headless/锁屏/Wayland host | 同轨锁屏黑帧（`uu-remote-linux/src/locked_video.rs:1`）；Portal生命周期（`uu-remote-linux/src/portal.rs:1`）；uinput（`uu-remote-linux/src/uinput.rs:1`）；虚拟桌面验收（`uu-remote-linux/contrib/virtual-desktop/ACCEPTANCE.md:1`） | 作为未来Linux host参考：控制初始化不等待授权/首帧，桌面与独立工具分离，权限/锁屏恢复有状态机。当前客户端开发不顺带安装这些服务。 |
| U15 WebRTC vendor补丁边界 | data-channel接收容量（`uu-remote-linux/vendor/webrtc/src/data_channel/mod.rs:33`）；RTX路径（`uu-remote-linux/vendor/webrtc/src/rtp_transceiver/rtp_receiver/mod.rs:787`）；DTLS副本（`uu-remote-linux/vendor/webrtc-dtls/Cargo.toml:1`） | 数据通道接收缓冲修到262144，配套SDP max-message-size；但RTX循环仍只read然后丢弃。应按问题逐项评估补丁，不能整库替换后宣布修复RTX/FEC。 |
| U16 合成fixture与回归 | 合成H264/H265/444 fixture（`uu-remote-linux/tests/fixtures/viewer/README.md:1`）；握手/能力测试（`uu-remote-linux/src/viewer/tests.rs:1`）；payloader回环（`uu-remote-linux/src/viewer/media.rs:194`）；长稳审计（`uu-remote-linux/tests/fixtures/endurance-audit.py:1`） | 无真实桌面/账号数据的fixture，丢片/乱序/跨回绕/codec切换/关闭回收均可参考。本轮只阅读，未运行U的整套测试；本项目74项通过是另一套结果。 |

### 已知差异：必须记住，避免再次照抄错误

1. **中键**：本项目原代码把egui `middle_clicked()`结束后的点击合成为`button=3`的press/release。既不是真实按住状态，值也不被当前Windows UU路径接受。实测3/4无中键事件，16有完整`Middle down → drag → up`。修复点为[button / RemotePointer](../crates/uu-gui/src/input.rs)；验证只覆盖当前Windows协商与目标，不推断所有UU平台都用16。
2. **自定义码率**：该快照U的host仍把field8直接存入custom_mbps，旧交接也曾按Mbps填该字段；Windows抓包证明当前路径应为bits/s，且quality=6、外层timestamp和完整setting字段影响生效。本项目[CaptureSettingRequest](../crates/uu-core/src/control_wire.rs)是已验实现；不能退回简化版字段。
3. **重排容量**：U的32包与本项目2048包/60ms不是同一压力量级。1080p大IDR能拆成数百包，容量上限可能比40ms定时更早触发丢弃。参数必须按包率和突发测量，不能只比较毫秒值。
4. **不要随意丢参考帧**：U的Latest用于AU和图像不同层时，语义并不相同。显示图可换成最新值；压缩P帧直接丢弃会损坏后续参考链。本项目8 AU队列过载进入IDR恢复，追赶时保留解码、跳旧RGBA转换。
5. **延迟/吞吐指标的口径**：M的系统网络用量不一定等于本次串流带宽；协商FPS不等于decoded FPS；当前本项目decode_ms也不包含颜色转换/上传/显示。当前 RTT 已使用 CONTROL 实测均值及偏差；未取得样本时显示缺失，不用 0 宣称零延迟。
6. **能力广告必须真实**：U有HEVC/444和多工具，不表示本客户端已有相同解码/呈现/协议链路；S有PyroWave/HDR/USB，不表示UU官方host接受这些扩展。

## 6. 可实施顺序与验收标准

| 优先级 | 独立功能范围 | 参考 | 本项目接入点 | 完成判据 |
|---|---|---|---|---|
| P0 | 可重复网络/参考链回归与分阶段指标 | S08、N02、U16 | `video_receiver`、`media`、HUD/probe | 同一合成码流下重排/重复/随机与突发丢包；统计NACK后真正补回包、坏AU、IDR恢复时长p50/p95、decode/convert/upload/present、队列深度；含停止和重连；不得记录键盘/剪贴板/凭证 |
| P0 | 确认官方修复格式，再实现适用的RTX/RED/ULPFEC | N01、U15 | WebRTC封包/codec注册、接收层 | SDP匹配与真实RTP证据；RTX恢复OSN/原PT/原SSRC并进入重排路径；同一丢包样本开关对比有效恢复计数、坏帧和停顿；没有发送端支持时如实标未支持 |
| P1 | Windows GPU纹理呈现 | M02 | `uu-media` surface合同、GUI renderer | Intel/AMD真实硬解，1080p裁剪/stride、缩放/DPI/resize/设备丢失/回退均正确；减少CPU回读与上传，记录CPU占用及呈现耗时，不只看decode_ms |
| P1 | 按源PTS与显示刷新调度 | M01 | frame mailbox、UI重绘、renderer | 30→60/120Hz、60→60/144Hz、VSync开关比较帧间隔p95/p99、排队延迟、撕裂；不再固定16ms定时等同于pacing；静态画面不空耗重绘 |
| P1 | 纯规则自动码率 | S06、M07 | stats→决策→CaptureSettingRequest | 基于窗口增量/有效RTT/队列，快降慢升、冷却期与用户上限；通过host设置回报/实际码流确认应用；和PLI请求分开，避免IDR风暴；保留手动模式 |
| P2 | 音频、光标、完整键盘、剪贴板 | M03–M06、U06–U08/U12 | 独立输入/音频/平台bridge | 每个功能单独提交；输入含右修饰键/功能键/侧键/失焦/黑边/按钮顺序；音频含抖动与设备切换；光标有hotspot/隐藏，剪贴板无回环 |
| P2 | 文件→终端→端口映射工具 | U09–U11/U15 | 按capture_type独立controller会话 | 先协议与取消/断开，再GUI；操作不阻断视频、不跨会话释放、不无限缓存；分别实测后逐项启用按钮 |
| P3 | HEVC/444/HDR、自有host与虚拟屏 | U05、M09、S05/S09–S12 | codec能力、像素格式、采集/编码后端 | 真实编码→传输→硬解→颜色正确呈现闭环；按能力降级，不先虚报支持；PyroWave/RS需要可控制的两端 |

以上是后续实施路线，不表示本轮已经完成RTX/FEC、直接GPU呈现或完整帧节奏。

## 7. 本轮已经落地的对应点

| 提交 | 完成功能 | 验证 |
|---|---|---|
| `9c20bbd` | Windows完整CaptureSettingRequest和bps单位 | 抓包字段对照、自定义码率单测；独立暂存快照workspace编译通过 |
| `6c90b7c` | 硬解选择/软回退，NV12裁剪、帧probe | Intel Arc B390与AMD RX7900XTX实测；media30+CLI3测试通过；VAAPI仍缺真实GPU验收 |
| `e66bff8` | RTP重排/严格AU、有界解码/显示、按损坏恢复 | core17+回环2+media31测试通过；Windows实测约30源/29解码FPS的一个窗口，不作长期保证 |
| `43a259d` | 中键16，真实down/hold/drag/up，失焦松键 | GUI2测试；Windows端接收事件验证 |
| `ad89c4e` | 参考UU图重做主页与串流控件 | 原生预览；Windows高DPI/硬软解与输入测试；只读/设置弹窗阻断输入 |
| `d3219d1` | Windows分发包和此前实现–07 | 最终workspace74测试通过；Windowsrelease构建与Windows 测试客户端包哈希核对 |

当前平衡恢复仍可能在丢参考帧后短暂显示预测误差；严格等IDR在实测网络中会明显降低连续性，因此保持可选。软解在高码率/丢包压力下仍有停顿。后续优先改可验证的修复链路和GPU呈现，不能把暂时不报decode error当成没有可见花屏。

本轮测试只连接获准的Windows 测试主机。Windows 测试客户端客户端测试地址为`192.0.2.10:22`；测试约束及清理记录见此前实现。

## 8. 快速迭代时如何更新索引

先读取各仓库的新HEAD、工作区状态及变更路径，再按编号重新读受影响代码；只在验证后更新“已落地”。不要自动pull用户的参考工作区，也不要把其未提交改动当成固定版本。

```sh
git -C uu-remote-linux status --short
git -C uu-remote-linux rev-parse HEAD
git -C uu-remote-linux diff --stat b7f9f2aa683afb40e9b1bc95db88acf50fd69f85 HEAD -- src/viewer src/capture_setting.rs src/input.rs src/input_mapping.rs src/video_track.rs src/cursor.rs vendor/webrtc vendor/webrtc-dtls tests

git -C foundation-sunshine diff --stat c4b9a3f5f1bb604f081c37a2e6e6df42b7d47f9f HEAD -- src/streaming src/stream.cpp src/video.cpp src/platform/windows src/pyrowave tests/unit

git -C moonlight/moonlight-qt diff --stat 914a4d74acea09e8c5f1e283c83c676f73fa9c34 HEAD -- app/streaming file-mapping moonlight-common-c tests
git -C moonlight/moonlight-qt submodule status
```

本地链接行号对应上述快照，后续以符号名重新定位。common-c更新要同时检查M的gitlink和实际子模块HEAD；不得拿另一个分支的恢复协议默认当作M已集成。每完成功能执行适合该改动的验证并立即git提交，提交描述保留来源快照、适配范围和验收结果。

## 9. 日常使用 demo 增量（2026-10-19）

参考快照保持不变，以下列出已实现功能及验证范围。

| 参考入口 | 当前落地 | 验证范围 |
|---|---|---|
| M04、U06/U12 输入 | `2b94f19` Windows原生VK统一窗口/全屏，物理键后备；`0f53e29` 桌面绝对/游戏相对和两种光标 | 用户确认窗口/全屏字母与标点正常；光标和游戏仍待完整验收 |
| M05、U07 音频 | `e4fe38f` 独立Opus+WASAPI，后续有界抖动、FEC/PLC、设备重开、MMCSS及过渡平滑 | 用户确认播放正常、延迟可接受；卡顿爆音平滑待验；不能宣称每包都有FEC |
| M06、U08 剪贴板 | `c97b805` 前台窗口独占双向文本同步、30KiB、无回环和超时清理 | 真实WebRTC TEXT回环通过；未操作用户系统剪贴板；不含图像/文件 |
| M03 与软件交互 | Q/X/Z快捷键、80%跨屏窗口、独立主页、多设备、持久设置、取消/重连反馈 | 几何/状态测试和Windows编译；完整多设备与跨屏实机待验 |

没有直接复制GPL应用实现；参考协议与机制后适配现有Rust架构。视频FEC、GPU直接呈现、完整音视频时钟同步仍属后续工作。

## 10. 远端多显示器

采集源切换：旧采集源切换返回Message8 CaptureChange，不保证同时刷新ScreenSources；消费该通知恢复轨道0的源绑定，再等待设置确认与目标画面，避免无限等清单。GUI对稳定绑定复用纹理/确认过的画质，标签支持本地拖动和按设备持久化，编号与排列分离。

未绑定轨道：`Screen.12`为int32，负数是未绑定轨道，不能转成u32后按五路数量限制拒绝。未绑定源先发 `Message.9 CaptureConfig(client=3,screen_id=真实源ID,config_flag=8)`，等待 `Message.19` 同序号的flag8成功回复和清单绑定，再发送目标采集设置并等待新画面。此路径只激活已有原生屏幕，不创建虚拟屏或修改显示布局。SDP的 `video_N` 决定轨道身份，m-line容量不代表显示器ID上限。

显示器布局策略与串流源选择分开。`screen_control`解析官方名称/主屏/真实源ID/轨道；普通切屏使用 `RpcRequest.15 SendVideoTrackRequest` 加目标屏幕的采集设置，等待两种确认和新画面。顶部标签及+列表参考用户 `uu_menu_top.png`，全屏沿用同一清单。原来的纹理列表不能代表所有可用屏幕。

本轮直接核对本地官方Windows描述符和被控端静态处理，确认 `screen_id=-2` 在多轨道时广播现有轨道，纠正此前“保持当前源”的注释。独占使用 `RpcRequest.26 EnterSuperScreenRequest`、reason=1、实测尺寸，退出使用case14；响应分别为22/10，切轨响应case11。这里只列自实现的字段对应关系，不导入官方二进制或GPL应用代码。普通模式不发送这些布局RPC，也不会自动修复既有的独占布局。

## 11. 原生输入后端与标准键码

自有主机的输入后端使用 `keycode` / `keycode_macro` 1.0.0 的完整跨平台映射，避免把 USB HID usage 直接当成 Windows 扫描码或 Linux evdev 编号。来源为 [keycode](https://github.com/dfrankland/keycode) 与其固定的 [Chromium 键码数据](https://chromium.googlesource.com/chromium/src/+/2b6022954b9fb600f15e08002a148187f4f986da/ui/events/keycodes/dom/keycode_converter_data.inc)；Rust crate 的 MIT 许可和数据的 BSD 版权/许可均随第三方通知保留。

`uu-input` 提供 Windows SendInput 与已授权 X11 XTest：明确转换扩展扫描码、侧键与横向滚轮，按物理虚拟桌面坐标处理负屏幕原点，并只释放本会话成功按下的键与按钮。Linux 普通键鼠不默认打开内核输入设备；Wayland 键鼠由同一采集门户会话授权和执行，详见[原生采集](native-capture.md)。可选虚拟手柄的 UHID / ViGEmBus 依赖另见[手柄说明](native-gamepad.md)。

后端验证使用有认证的独立 Xvfb，检查中键/侧键、滚轮、绝对/相对移动、Shift 标点与释放归属；Windows 单元测试只检查输入字段与生命周期，不向用户桌面注入测试按键。**这属于原生控制的底层准备，v0.2.0 原生观看仍然只读**，不能把后端组件视为已接入远控。

### 自有音频通路补充

| 可参考方向 | 当前实现与边界 |
|---|---|
| Sunshine 的系统输出采集分层 | 独立 Rust WASAPI loopback / PulseAudio monitor 后端，采集和编码留在自己的 OS 工作线程，默认不共享声音。 |
| Moonlight 的实时音频与陈旧数据处理 | 有界队列、过期块丢弃、短渐变及解码与播放分离；设备错误保持视频，后台重新打开采集。 |
| 标准音频冗余 | 独立实现 RFC 2198 一帧 RED，保留 Opus FEC/PLC 回退；与 Sunshine 的 GameStream 视频 FEC 不同，未复制 GPL 代码。 |

相关模块为 `audio_capture`、`audio_red`、`native_audio` 与 `native_media`，能力与验证方法见[原生音频](native-audio.md)。

### 自有主机采集与 CPU 编码开销

Sunshine 的 `src/platform/windows/display_base.cpp` 在帧节奏等待后以零超时查询桌面更新；本项目据此检查重复等待，独立调整 `DxgiCapture`：只有首帧使用短等待，已有缓存时非阻塞查询并复用静态画面。生产线程仍按目标帧率等待，避免空转。没有导入其 GPL 源码。

`uu-media::convert::bgra_to_i420` 使用独立定点 BT.601 全范围运算，2×2 色度先合并再舍入；与旧浮点公式的随机差分测试限制到每通道 1 个色值。`convert_bench` 提供不接触桌面的离线转换基准；主机 CLI 的阶段耗时用于区别采集与编码瓶颈。软件编码仍受画面内容与 CPU 性能限制。


## 0.9 客户端呈现进展与剩余边界

候选已实现原始 CPU 平面邮箱、GPU 颜色转换和纹理复用、Windows 有界读回存储复用、500ms HUD 快照，以及可用时的 ICodecAPI 低延迟提示。具体测量范围见[Windows 延迟说明](windows-latency.md)。这减少了 CPU 处理，但 M02 的 GPU 表面直达仍待实现。

复核 M02 时应一起阅读 D3D11 renderer 的设备选择（约213–450行）、renderVideo（约1151行）和交换链说明（约660–710行）：

- 解码与渲染设备可共享，也可使用同适配器的双向 fence；部分驱动直接绑定解码表面，其他路径使用显存内拷贝。避免 CPU 回读不等于所有 GPU 都完全不拷贝。
- 保留图像与采样器表面的所有权，直到 GPU 使用完成，避免解码器提前复用参考表面。不能把裸句柄塞入 Bytes 来代替生命周期与同步合同。
- 其翻转交换链、外部帧节奏与 SyncInterval 策略共同工作。源码特别说明盲设最大帧延迟为 1 可能使 DWM 阻塞；不能把常量搬到当前 OpenGL 后端，也不能通过关闭用户的 VSync 来冒充优化。

下一阶段应先定义带类型的 GPU 表面、设备标识、可见裁剪、颜色、PTS 和完成信号，再选择能真正消费它的 Windows 呈现后端。首个原型可先在无窗口 D3D11 设备上验证生成的图像，随后在已授权的独立显示环境检验窗口呈现；非交互 GL 配置不可用时不桥接工作桌面。

### v0.18 原生视频保护更新

本项目两端新增 [有界 XOR 视频 FEC](native-fec.md)，只在原生 SDP 双方确认后启用，真实发出校验并将重建 RTP 送回重排器。每组最多 8 数据包，恢复一个缺失；校验预算含固定封装估算最多为编码负载 20%。这是本项目私有扩展，未复制 Sunshine RS 代码，也不等同于 RS/ULPFEC 或官方 UU 手机协议；上文涉及“未实现视频 FEC”的记录保留其历史范围。
