# 原生视频编码后端

此接口用于 Eclipse 原生主机，不影响 UU 兼容模式远端选择的编码器。Linux 使用 VA-API；Windows 自动模式在兼容 Intel 采集设备的 SDR 串流上优先使用 oneVPL 显存输入，AMD 适配器优先使用原生 AMF，其它适配器或纹理初始化失败时可使用驱动注册的 Media Foundation 编码器；显式 Intel 使用 VPL。硬件格式支持 H.264、HEVC Main 8-bit 和 AV1 Main 8-bit，以实际驱动初始化结果为准。

在被控端打开服务端设置的编码页即可选择编码器。图形界面默认自动选择硬件。命令行的 `native-host`、`lan host` 和 `internet host` 默认使用软件编码，支持 `--encoder software|auto|hardware|vaapi|intel|amd|nvidia`。`hardware` 要求硬编成功，`vaapi` 用于 Linux。

H.264 CBR 自动模式在硬件初始化失败时可回退 OpenH264，CPU 输入运行失败时从新的 IDR 参考链继续；GPU 输入运行失败会明确报错，避免将缺少 CPU 像素的纹理交给软件编码器。界面报告实际后端；诊断日志保存具体硬件失败原因。必须硬件模式直接报错，不用软件成功掩盖硬件失败。非法码率在调用驱动前被拒绝，当前编码器保留。

## 按硬件调节串流延迟

先在串流诊断中查看实际编码后端，再在被控端修改对应选项。保存服务端设置并按界面提示重启共享后，新连接使用所选参数。客户端仍可选择编码格式、分辨率、帧率和码率。

| 设置 | 适用路径 | 用途与默认行为 |
| --- | --- | --- |
| 硬件关键帧间隔，1–60 秒 | MFT、oneVPL、AMF、VA-API | 较长间隔减少周期性大帧，较短间隔提供更多独立恢复点。默认 MFT、oneVPL 和 VA-API 为 2 秒，AMF 按需发关键帧。首帧和丢包恢复可提前请求关键帧。 |
| 画质 / 速度，0–100 | Windows Intel MFT、NVIDIA MFT | 0 偏向速度，100 偏向画质。默认尽量申请 0。显式设置会核验驱动回读。Intel 在自动或硬件模式下可以保留 D3D11 输入。 |
| Target Usage、低功耗引擎、初始 VBV | Intel oneVPL | Target Usage 为 1–7，7 偏向速度。显式 Intel 或修改 oneVPL 参数时使用该后端。Windows 上的 AV1 CBR 默认让驱动决定 VBV，串流诊断显示驱动返回的 kB。过小的 VBV 会导致静止文字在周期关键帧后逐渐变糊。其它组合默认尝试 3 帧 VBV。显式指定 1–5 帧会覆盖默认值。Windows 默认开启「优先使用 D3D11 纹理输入」。同一块 Intel GPU 上完成采集画面的颜色转换和编码，诊断显示 `D3D11 input`。其它情况显示 `system memory`。 |
| 质量预设、输入队列容量、初始 VBV | Windows AMD AMF | 支持速度、均衡、画质。输入队列可设 1–16 帧，VBV 可设 1–5 帧。两者默认交给驱动。队列容量表示驱动允许接收的上限，实际在途帧数取决于提交和输出。 |
| 空间自适应量化 | Windows AMD AMF | 支持自动、启用、停用。H.264 和 HEVC 使用 VBAQ，AV1 使用 CAQ。它会改变同一帧内的码率分配。 |

VBV 约束编码器允许的码流突发。以 48 Mbps、60 FPS 的 CBR 为例，一帧预算是 800,000 位，约 100 kB。VBR 按初始峰值计算，oneVPL 按驱动的 kB 单位向上取整。同一编码会话中调整目标码率会保留初始缓冲容量。较小的预算可能降低复杂场景或关键帧的画质。MFT 的缓冲属性随编码格式使用不同单位，因此界面只在 oneVPL 和 AMF 下提供这项设置。[微软缓冲属性](https://learn.microsoft.com/en-us/windows/win32/codecapi/avenccommonbuffersize-property) · [AMD 编码属性对照](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Recommended%20FFmpeg%20Encoder%20Settings)

### Intel AV1 离屏实测

在一台 Windows Intel Arc B390 上，使用相同的动态纹理、SDR、AV1 CBR 48 Mbps 和 60 FPS 目标。每档按 MFT、oneVPL、oneVPL、MFT 的顺序各编码 240 帧。两条路径都从 GPU 上的 BGRA 开始，计时包含颜色转换、提交和等待本帧编码返回。

| 分辨率 | Intel MFT 编码 P95 | oneVPL D3D11 编码 P95 | 每秒编码帧数，MFT → oneVPL |
| --- | --- | --- | --- |
| 1920×1080 | 9.66–11.19 ms | 5.39–5.52 ms | 60.1 → 60.2 |
| 3840×2160 | 24.28–27.83 ms | 14.46–14.71 ms | 32.6–39.8 → 60.0 |

4K 时 MFT 实际输出 26.5–32.4 Mbps，oneVPL 输出 49.2 Mbps。较慢的一组也使用相同的目标码率。数据可以复现该显卡编码环节的收益，采集、网络、客户端呈现和显示器扫描需单独测量。

#### 跨关键帧检查 AV1 CBR 文字清晰度

一次 90 帧的色条测试只覆盖约 1.5 秒。检查静止文字时，需要跨过多个周期关键帧。将清晰的桌面文字测试图导出为 1920×1080 的紧密排列 BGRA 文件，运行下面的离屏探针。它先显示空画面，随后显示、滚动并停住同一页文字。每次校验都从实际解码表面取回像素，并与该帧的输入比较。

```sh
encoder_probe --encoder auto --codec av1 --rate-control cbr --fps 60 \
  --bitrate-kbps 30000 --width 1920 --height 1080 --frames 720 --steady true \
  --input gpu --scroll-bgra text.bgra --decoder hw --verify-every 8
```

在 Intel Arc B390 上，旧的三帧 VBV 会使周期关键帧的量化逐步加重。驱动自行选择 VBV 后，仍使用硬件 CBR、单帧异步深度和同设备表面；在线改码率及请求恢复关键帧沿用同一套接口。

### 与 Foundation Sunshine 的参数对照

| Sunshine 编码器 | 源码提供的主要调节 | Eclipse 对应能力 |
| --- | --- | --- |
| Intel QSV | 编码速度预设、低功耗 HEVC 引擎、H.264 熵编码选择。实时路径使用单帧异步深度。 | oneVPL 可选 Target Usage、低功耗引擎、VBV 和 Windows 同设备 GPU 输入。H.264 目前协商 Baseline，使用 CAVLC。 |
| AMD AMF | 质量预设、用途、码率控制、预分析、自适应量化、HRD、参考帧与切片数量。 | 支持速度 / 均衡 / 画质、CBR / VBR、GOP、初始 VBV、输入队列和空间自适应量化。采用超低延迟用途。 |
| NVIDIA NVENC | P1–P7、单遍 / 双遍、空间 AQ、VBV、lookahead、时间滤波、分帧编码、帧预算保护。 | 当前 Windows 路径是 NVIDIA MFT，可调画质 / 速度和 GOP。P1–P7、双遍与分帧编码需要原生 NVENC 接口。 |
| 软件编码 | `sw_preset`、`sw_tune` 可选择编码库提供的速度与延迟策略。 | 软件 H.264 使用 OpenH264，编码器参数取自该库。 |

涉及后续帧的预分析会延后当前帧输出。Eclipse 的实时采编路径按输入帧取得对应编码结果，保留首帧和丢包恢复时的关键帧请求。[Foundation Sunshine 配置解析](https://github.com/AlkaidLab/foundation-sunshine/blob/07d8ae13c658128d267c357196a610fb8b2667f3/src/config.cpp) · [Sunshine 视频后端](https://github.com/AlkaidLab/foundation-sunshine/blob/07d8ae13c658128d267c357196a610fb8b2667f3/src/video.cpp) · [Sunshine 原生 AMF](https://github.com/AlkaidLab/foundation-sunshine/blob/07d8ae13c658128d267c357196a610fb8b2667f3/src/amf/amf_d3d11.cpp) · [Intel VPL 编码参数](https://intel.github.io/libvpl/latest/API_ref/VPL_structs_encode.html) · [微软画质与速度属性](https://learn.microsoft.com/en-us/windows/win32/codecapi/avenccommonqualityvsspeed-property)

三个主机 CLI 入口均支持 `--encoder-tuning`，参数采用与服务端配置相同的 JSON 对象。例如在 Bash 中使用 `--encoder hardware --encoder-tuning '{"hardware_gop_seconds":10,"intel_mf_quality_vs_speed":0}'`。手动还原时关闭对应开关，即可沿用该后端的默认策略。

## Windows Media Foundation

Windows 默认构建包含 `mf-encode`，按 H.264 / HEVC / AV1 子类型枚举注册为硬件的 MFT，不把系统软件 MFT 当作显卡编码。使用驱动的异步输入/输出事件，设置低延迟、无 B 帧、实时编码参数以及 CBR / 峰值约束 VBR；编码前后检查当前帧时间戳，避免积攒旧帧。驱动需随显卡正式驱动安装并启用。

COM、Media Foundation、编码器和缓冲均在专用线程创建、使用和释放，与 Linux 共用有界请求队列和超时处理。NV12 直接写入每帧独立的 MF 输入缓冲，省去临时 NV12 向量到输入 sample 的整帧复制；异步编码器持有 sample 时，其缓冲保持独立，不根据输入额度提前复用。恢复关键帧携带实际编码器生成的参数集，核验尺寸、颜色和当前帧时间戳。支持显式请求关键帧与动态码率调整。支持 D3D11 输入的编码器直接接收 GPU NV12 sample，CPU 输入保留逐帧独立缓冲。

MF 输出的 HEVC SPS 和 AV1 序列头会明确声明本程序实际送入的颜色信号：SDR 为有限范围 BT.601，HDR10 为有限范围 BT.2020 / PQ。驱动忽略媒体类型中的颜色属性时，只修正本机编码器的序列描述，保留编码图像、参考关系和其它参数；接收端码流不做这种改写。输出仍须通过尺寸、位深和颜色校验。

驱动内部等待在 Windows 默认计时精度下可能多消耗一个刷新量级。编码器活动期间持有配对的 1 ms 计时请求，退出时释放；不会修改系统设置或目标帧率。对支持的驱动申请桌面远程场景和速度优先配置。开发诊断可显式设置 `UU_ENCODE_PROFILE=1`，退出时输出转换、输入提交、驱动完成的聚合耗时；默认关闭，不记录画面。

实现依据微软的 [硬件 MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/hardware-mfts)、[异步 MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts) 和 [H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder) API。Sunshine 与 uu-remote-linux 提供了硬件选择、低延迟参数和恢复行为的参考；本后端未复制其 GPL 代码。

## Windows GPU 输入与 AMD AMF

支持 GPU 输入且适配器匹配时，DXGI 采集把桌面纹理复制到会话独立持有的两个 GPU 快照槽；只有强、弱租约全部释放的槽才能复用，两个槽都在使用时跳过新快照，尺寸或桌面失效后清空池。D3D11 视频处理器在 GPU 上完成输出缩放和 BGRA→有限范围 BT.601 NV12 转换，再交给编码器。不会把这些像素下载到 CPU 后重新上传。复制和转色仍会消耗 GPU 时间，不等于完全没有复制。旋转屏、编码器不支持 D3D11 输入或跨厂商设备保留既有 CPU 路径；物理输入坐标始终描述原始屏幕。

AMD 使用安装在系统目录的 AMF 驱动运行时，通过 `InitDX11` 和 `CreateSurfaceFromDX11Native` 接收纹理。采用超低延迟用途、速度预设、无 B 帧和有界输出等待；每个 sample 独立持有其纹理，直到编码器完成。H.264 / HEVC / AV1 共享生命周期、CBR / VBR、关键帧恢复与动态码率规则。显式 AMD 失败时返回具体原因，不偷偷选择其它厂商。API 头文件来源及固定版本见 [AMF API headers](../../vendor/amf-api/README.md)，接口语义见 [AMD AMF SDK](https://github.com/GPUOpen-LibrariesAndSDKs/AMF)。实现参考 Sunshine 的 GPU 输入和输出生命周期，未复制其 GPL 实现。

编码准备现在可保留采集设备的类型化引用，并直接在同一个 AMD D3D11 设备上初始化 AMF。这样不再先建立默认设备的编码器、收到首个采集纹理后又重建一次；初次连接、能力探测和需要重建编码器的帧率修改使用同一规则。租约构造拒绝单线程 D3D11 设备并确认 immediate-context 多线程保护；像素仍归各自帧所有。跨适配器和其它后端保留已有选择与回退，不把不同设备上的裸纹理直接互用。设备绑定思路参考[Foundation Sunshine 的采集设备初始化](https://github.com/AlkaidLab/foundation-sunshine/blob/0219abe22c4d9c937a6ca52ebb895f216da4b077/src/platform/windows/display_vram.cpp#L3368)，代码独立实现。

在同一 AMD 设备上，以 GPU 直接生成的4K色条交替执行“关闭/开启采集设备预绑定”，H.264 / HEVC / AV1 每种各两轮；准备与首帧编码合计约从102–112ms降至38–45ms，首帧编码约从46–50ms降至19–26ms。计时同时保留准备和首帧，避免只将耗时移出编码调用。这不包含捕获设备建立、桌面采样、网络、解码或显示，不是端到端延迟。三格式另外各完成120帧GPU硬解、30帧二倍缩放硬解，定期显式读回比较像素；非校验帧保持GPU租约。

捕获快照池的4K原生对照按“新分配 / 复用 / 新分配 / 复用”执行，每组600帧。采集复制加编码的中位耗时：H.264 9.80/9.90→8.29/8.26ms，HEVC 9.71/9.80→8.24/8.29ms，AV1 8.87/8.83→7.29/7.31ms；每种格式四轮的输出哈希一致。输入为GPU直接生成图案，不采集桌面，不包含网络与显示；池不复用仍被异步编码器持有的 NV12 sample。

AMF 三种格式的默认 GOP 周期为 0。启用自定义关键帧间隔时，Eclipse 按串流帧率换算成帧数，在初始化后核对驱动实际值。公共编码线程仅在首帧、编码器重建和显式恢复请求时要求关键帧；AMF 在线目标码率调整保留参考链，不额外请求 IDR；需要重建编码器的模式、VBR峰值等参数变更仍以新关键帧开始。需要重建参考链的其它后端仍可要求关键帧，不能把这条策略当成所有后端的无限 GOP 保证。

在一台 AMD GPU 上，同为 3840×2160、60 FPS 设置、14 Mbps、60 帧生成色条的测试中，MFT CPU 输入均值约 43.4 ms，MFT GPU 输入约 45.6–45.8 ms；原生 AMF CPU 输入约 13.7 ms，AMF GPU 输入两次约 8.9 / 9.1 ms，中位约 8.0 ms。AMF HEVC GPU 输入约 9.0–9.2 ms。结果包含编码调用的转换、提交与驱动等待；生成纹理的初始 CPU 上传在计时外，真实桌面采集、传输和呈现也不在该计时内。测试核验颜色、冷关键帧、降码率及恢复，不能等同于真实桌面总延迟验收或所有显卡的性能保证。

非默认64×16对齐的 AV1 尺寸使用 AMF 的无额外对齐限制模式；驱动仍可能在码流右侧或底部补边，例如640×360输出640×362、1080行输出1082行。原生会话以已确认的显示尺寸裁剪解码输出，不拉伸、不修改远端桌面。D3D11 裁剪只建立保留原 sample / 快照父租约的视图，DMA-BUF 沿用原存储；CPU 输出按原平面偏移裁剪。解码器重建仍保留会话显示尺寸。此行为依据 [AMF AV1 对齐说明](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/AV1%20Encoder)。

AV1 时序头包含标准的 32-bit 无符号字段。公共位读取器支持完整位宽，避免将带时序信息的有效序列头误判为不支持；保留有符号读取的边界检查。

## Linux VA-API

复用已固定的BSD许可cros-codecs/cros-libva API，不依赖外部FFmpeg进程。驱动、上下文和表面由同一线程创建、使用、释放，请求队列有界。选择可用的H.264 Baseline低功耗或常规编码入口，帧率请求沿用公共编码规则，实际吞吐取决于格式、尺寸和驱动。

采集帧仍经过CPU转换与上传。输入使用有限范围BT.601 NV12，编码SPS必须与它一致；支持映射NV12表面的驱动避免额外的图像读回/写回，不支持时使用复制上传。输出为CPU Annex-B码流。这还不是完整GPU管线或零拷贝。

恢复请求需要IDR与SPS/PPS。局部H.264预测器补丁将显式关键帧请求作为新序列起点，避免仅发普通I帧后仍依赖旧参考链。强制恢复帧使用全新软件解码器验证，不能只看持续解码器是否有画面。

## Intel VPL：HEVC、AV1 与显式 Intel 编码

Windows 编码初始化先按实际 Intel 适配器使用 dispatcher 的快速硬件会话路径，再通过编码查询、初始化和参数读回确认所选格式与画质。这样无需为一次失败的格式尝试扫描所有运行库的完整能力；适配器或运行库不支持该初始化路径时仍使用完整搜索。明确指定的编码格式继续实际尝试，失败不会被静默改成其它格式。快速路径依据随包固定的 [Intel dispatcher 实现](https://github.com/intel/libvpl/blob/674d015bcb294bc39fa276e99a652ea045423e82/libvpl/src/mfx_dispatcher_vpl_config.cpp)。

编码库还提供 Intel VPL 硬件后端，支持驱动实际具备的 H.264、HEVC Main 8-bit 和 AV1 Main 8-bit 编码，以及 CBR / VBR。Windows 自动模式在兼容 Intel SDR 采集设备上优先尝试 oneVPL 纹理输入，随后可按适配器与格式尝试 AMF、MFT 或系统内存 VPL；Linux H.264 CBR 优先使用 VA-API；显式 Intel 以及 Linux HEVC / AV1 / VBR 使用 VPL。在服务端设置中配置默认编码、允许的格式、码率模式和 VBR 峰值；IP、UU 连接服务、自建连接服务共用这些设置。控制端仅在生成测试流实际解码成功后通过 SDP 声明 HEVC / AV1 支持。双方都自动时按 **AV1 > HEVC > H.264** 顺序尝试主机允许且两端共同支持的格式；服务器默认编码可以选择自动。客户端明确选择的格式优先于服务器默认值，但仍须在服务器允许列表内；客户端自动时才使用服务器的明确默认格式；只有双方自动才按格式优先级回退。首个 Answer 不额外初始化未选用的编码器。显式格式初始化失败会报告原因，不静默换成 H.264。既有配置的明确格式保留，新配置默认为自动。明确请求的格式不受支持或驱动不满足请求时，连接显示准备失败原因；UU 兼容模式的主机编码选择仍由其服务端决定。

VPL 使用单帧异步深度、无 B 帧、速度优先和有限 VBV；低延迟 BRC 等额外调优由驱动能力决定。初始化后再次读取实际配置，若驱动把帧率、尺寸、编码或码率模式改成不符合请求的值，直接拒绝。仅接受硬件实现，拒绝部分软件加速。恢复关键帧携带实际编码器的参数集，重新设码率后请求新的关键帧；不会伪造参数或把 HEVC / AV1 失败伪装成软件 H.264 成功。

VBR 设置目标和峰值码率，峰值必须不小于目标。CBR 是编码器速率预算，不能保证每一帧、每一秒或加上网络协议后的线上字节数完全恒定。软件 OpenH264 路径保留平均码率控制，不提供这个 VBR 后端。

构建只使用固定版本的 MIT Intel 公共头文件和 C 编译器。运行时需要 Intel GPU 对相应编码的硬件支持及驱动：Linux 安装 Intel VPL dispatcher（`libvpl.so.2`）和 GPU runtime；Windows 正式包随带 MIT 许可的标准 `libvpl.dll` dispatcher，仍需系统 Intel GPU runtime；直接 Cargo 构建时需自行提供 dispatcher。硬件实现由标准 VPL dispatcher 查找，应用不自行扫描驱动目录或直接加载私有实现 DLL。不要求链接 FFmpeg 或启动编码子进程。系统内存输入使用 CPU 转 NV12 并上传。Windows 开启 D3D11 输入后，采集纹理在同一设备上转换并复制到 oneVPL 自有表面。API 依据 [Intel VPL 低延迟说明](https://intel.github.io/libvpl/latest/appendix/VPL_apnds_c.html) 和 [编码过程](https://intel.github.io/libvpl/v2.10/programming_guide/VPL_prg_encoding.html)，公共头文件来源和固定提交见 `vendor/libvpl-api/UPSTREAM.md`。

AV1 解码库在 Linux 复用 VA-API 表面池、DMA-BUF 租约与下载路径；Windows 可通过 Intel VPL 硬解为 CPU NV12 输出，之后仍有像素复制。H.264 / HEVC 的既有 D3D11 共享呈现路径保留。`av1_decode_probe` 使用生成的 I/P 时间单元，检查冷启动、连续解码、时间戳和可选 GPU 租约；仍需在目标驱动上实测。

开发时可用生成视频探针要求真实硬编，例如：

```sh
cargo run --release -p uu-media --example encoder_probe --features vaapi -- \
  --encoder hardware --codec hevc --rate-control vbr --bitrate-kbps 8000 \
  --peak-kbps 16000 --decoder hw --frames 60 --width 1280 --height 720
```

AV1 使用 `--codec av1`。`--decoder none --output generated.obu` 只验证编码并保存生成码流，报告 `decode_checked: false`，不能当作解码或交互验收。实际 GPU 编解码与性能需要在目标驱动上验证；隔离的 `tools/native/vpl_contract_smoke.py` 是 ABI/边界测试，报告明确标为模拟运行时，不使用 GPU。

## 验证

```sh
cargo test --release -p uu-media --features vaapi
cargo run --release -p uu-media --example encoder_probe --features vaapi -- \
  --encoder vaapi --frames 180 --width 1920 --height 1080
```

探针默认生成色条，可检查颜色、尺寸裁剪、恢复关键帧、码率调整和逐帧解码。性能对照可用固定种子的动态块和固定节拍，全程使用生成画面。Windows 使用 `--encoder hardware` 要求真正硬编；Linux 可用 `--encoder vaapi`。`--decoder hw` 同时要求硬件解码，不以自动软件回退代替硬件互通验证。使用 `--encoder software` 或 `auto` 检查其它选择；缺少硬件后端的构建保留软件路径。

单台支持VA-API的Linux设备上，720p、1080p、1080×1920竖屏与3840×2160均完成恢复和色彩检查。合成色条的720p调用均值约为软件11.2 ms、复制上传VA-API4.4 ms、映射上传VA-API3.4 ms；4K映射上传约23 ms。这些是编码调用时间，包括转换、上传与等待，不是端到端延迟，也不是帧率保证；4K的该测量尚不足以持续60 FPS。不同码率控制会产生不同的实际字节数，不能将目标码率相同当作输出完全等价。

主工作区测试与实机探针通过。上游依赖的整包开发测试夹具使用过期后端接口，在未修改的固定依赖上同样无法编译；不将其报告为通过，H.264局部补丁以实际恢复探针验证。

## 0.13：减少 CPU 上传与编码等待

VA-API、Windows MFT 和 Intel VPL 共用直接生成有限范围 BT.601 NV12 的上传路径。BGRA 不再创建整帧 I420 中间结果，NV12 不再拆成 I420 后重组；I420 直接交织并转换范围。按 CPU 能力选择 AVX2、SSE4.1 或标量实现，支持非向量宽度、奇数字节行距和编码尺寸填充。完整检查源数据、目标行距、平面范围与重叠后才写入，只清理需要的编码边缘，不重复填充可见像素。

VPL 优先请求固定功能低功耗引擎，不支持时自动尝试通用硬件引擎；后端名称报告实际选择。动态改码率保留初始化后确认的 VBV 缓冲和初始延迟，避免 HEVC CBR 在降码率时因隐式调整缓冲而被驱动以 `Reset=-14` 拒绝。无 B 帧、单帧异步深度及关键帧恢复规则保持一致。

设置 `UU_ENCODE_PROFILE=1` 后，VA-API / VPL 退出时输出上传和编码完成的平均耗时；MFT 输出转换准备、提交和完成三段，转换准备包含 MF 输入缓冲分配与 sample 准备；比较旧版时应优先看编码调用总耗时，避免把阶段边界变化当作优化收益。上传段包含像素准备和平台上传同步，完成段包含提交、驱动等待和码流整理；它们用于定位编码调用的瓶颈，不包含屏幕采集、网络、解码或显示。

一台 Intel 核显设备上的生成画面测试中，4K BGRA H.264 编码调用均值从约 28 ms 降至约 24 ms；HEVC CBR 的驱动完成部分从约 22 ms 降至约 15 ms。该设备已通过 H.264 / HEVC 的 1080p、4K、三种输入格式、动态改码率、色彩及冷解码恢复检查。这些结果属于该 Linux GPU 的编码调用测量，不能据此承诺其它显卡、Windows 实机或端到端交互已达到同样延迟。当前仍有 CPU 读取和上传；进一步降低延迟需要按平台实现 GPU 采集、缩放、颜色转换与编码表面的连续传递。

## 0.8：复用 BGRA 转换

对按整像素对齐的 BGRA 采集帧，复用已有 OpenH264 的 `YUVBuffer` / `BGRA8Source` 接口，直接产生有限范围 BT.601 I420，再交织上传 NV12。该库按 CPU 能力选择 SIMD 或标量，缓存按编码会话复用。奇数字节行距与原有 I420 / NV12 输入保留原转换路径；输入布局在交给库前完整校验。

同一设备串行对比中，合成 BGRA 的 4K 编码调用均值由约49.1 ms降至27.6 ms（两轮交错对比），720p由6.46 ms降至4.10 ms；4K I420仍约22.2 ms。独立BT.601参考、黑白端点、透明度忽略、行距与填充检查通过，实机探针也验证竖屏、非16对齐裁剪、码率调整和恢复IDR。颜色转换的舍入可能与原路径略有差异，不承诺逐字节相同。

这些数据只描述合成输入的编码调用，仍包含CPU转换与上传，不能作为端到端或持续帧率保证，4K仍不足以承诺60 FPS。此项从0.8.0安装包开始交付，0.7.0仍使用原路径。

## 0.8：硬件互通与首帧

双硬件测试发现，原生 VA-API 编码虽选择 Constrained Baseline，SPS 却未声明相应约束，导致 VA-API 解码器拒绝并回退软件。编码端补齐约束位，并为实际的单参考、无 B 帧预测结构声明零重排；接收端保留参考面，按完整 AU 提交，不用清空解码参考链来强制出图。约束组合依据 [RFC 6184 表5](https://www.rfc-editor.org/rfc/rfc6184#section-8.1)。

编码探针增加 `--decoder hw`，直接打开硬件后端，失败即退出，不使用自动软件回退掩盖互通问题。它同时检查持续解码与带 Annex-B 零填充的恢复 IDR 能否交给全新硬件解码器，并保留全新软件解码器检查。

实际双 VA-API 合成串流完成600帧、零解码错误。另一轮单次首帧观测由约350 ms降至55 ms；这是接收端测试统计，不是玻璃到玻璃延迟。720p、4K、1918×1078裁剪和竖屏均通过无软件回退的硬件探针。硬解还会忽略已完成NAL后的合法Annex-B零填充，保留其它解析错误。

## Windows 验证范围

Intel Arc 核显已通过720p/1080p/4K生成画面的实际硬编与硬解、冷IDR恢复、码率调整、尺寸和色彩检查，后端明确报告 Intel Quick Sync Video H.264 Encoder MFT。该接口当前仍有CPU转换、缓冲复制和驱动等待，不能把硬编可用等同于端到端最低延迟。一次4K BGRA生成测试的编码调用中位约25–35ms，尚不能据此承诺持续4K60。

早期 AMD MFT 路径的冷 IDR 硬解曾出现不出首帧；当前 AMD 默认原生 AMF 路径已通过 H.264 / HEVC / AV1 生成画面的持续硬解与冷关键帧检查。该结果不改变其它 MFT 驱动仍需单独验证的范围。

## 原生协议与兼容

不带画质协商的旧入口保留 H.264 默认，Offer JSON 不增加旧主机无法识别的字段；格式能力放入标准 SDP。H.264 Answer 保持旧字段，非 H.264 连接才使用可选 codec 字段。HEVC 按 RFC 7798 单 NAL / FU 发送，AV1 按 AOMedia RTP 规范聚合和分片；三种格式共用完整访问单元、NACK / RTX、PLI、源时间戳和呈现管线。AV1 重组后恢复明确的 OBU 长度，不把 RTP 聚合头交给解码器。

新原生两端由客户端请求 5–240 FPS、码率和输出尺寸；运行中帧率与码率更新沿用同一媒体连接，分辨率与格式切换重新协商。服务器不再提供最大帧率、默认码率和最大码率设置；协议和编码接口仍校验安全数值范围、H.264 level 及硬件能力。旧协议继续保留兼容上限。CLI 三种主机入口均支持 `--codec h264|hevc|av1 --rate-control cbr|vbr --peak-kbps N`；非 H.264 或 Linux VBR 需选择 `--encoder hardware` 或 `auto`；Windows H.264 VBR 可由 MFT 硬件路径执行。新格式和 VBR 的真实性能还取决于目标硬件与驱动，不以接口模拟测试代替实机验证。

客户端请求及主机限制的完整规则见[客户端与服务端设置](../guides/client-and-server-settings.md#原生画质协商)。

## 多路编码准备和回退

新连接在授权后串行申请自己的编码器，并用实际采集输入验证关键帧输出后才返回准备成功。已有连接继续编码，不为检测容量额外启动一批试验编码器，也不把设备名称或 MFT 枚举数量当作并行路数上限。Windows 已知采集设备时直接绑定其 D3D11 设备管理器，不再先启动 CPU 会话再在首帧关闭重建。

新版两端通过独立 SDP 属性协商失败回退。客户端所选格式仍先尝试；失败后仅尝试主机允许且客户端实际声明可接收的其它标准格式。自动编码后端在硬件候选全部失败后，可以最后尝试软件 H.264 / CBR；明确要求硬件或特定厂商时仍保持硬件要求。PyroWave 保持显式选择，不进入标准格式回退。旧客户端未协商此能力时，明确格式及码率模式保持原规则。

回执记录请求与实际格式、码率模式，HUD 和编码菜单显示实际结果，并短暂提示自动回退。回退不会修改保存的偏好，之后的帧率或码率更新保留当前实际模式；用户再次选择编码模式时可重试。准备失败仅释放本次连接的资源。此机制处理驱动返回的失败，不等于对任意第三方驱动进程崩溃的完整隔离。
