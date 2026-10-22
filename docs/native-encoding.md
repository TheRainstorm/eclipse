# 原生视频编码后端

此接口用于 Eclipse 原生主机，不影响 UU 兼容模式远端选择的编码器。Linux 使用 VA-API，Windows 使用显卡驱动提供的 Media Foundation 硬件 H.264 编码器，包括支持此接口的 Intel 核显。

“共享本机”默认自动选择硬件，也可选择“硬件编码（必需）”或软件编码。旧邀请文件共享入口和 CLI 继续保留软件默认值；CLI 使用 `native-host --encoder software|auto|hardware|vaapi`，`uu lan host` 使用相同选项。`hardware` 要求当前平台硬编成功，`vaapi` 为 Linux 专用选项。

自动模式在初始化失败或运行出错时回退 OpenH264，并从新的 IDR 参考链继续。界面报告实际后端；诊断日志保存具体硬件失败原因。必须硬件模式直接报错，不用软件成功掩盖硬件失败。非法码率在调用驱动前被拒绝，当前编码器保留。

## Windows Media Foundation

Windows 默认构建包含 `mf-encode`，仅枚举注册为硬件的 H.264 MFT，不把系统软件 MFT 当作显卡编码。使用驱动的异步输入/输出事件，设置低延迟、无 B 帧和实时编码参数；编码前后检查当前帧时间戳，避免积攒旧帧。驱动需随显卡正式驱动安装并启用。

COM、Media Foundation、编码器和缓冲均在专用线程创建、使用和释放，与 Linux 共用有界请求队列和超时处理。关键帧必须为能独立恢复的 IDR，携带匹配尺寸和色彩的 SPS/PPS；输出为 H.264 Annex-B。支持显式请求关键帧与动态码率调整。当前输入仍由 CPU 转换为有限范围 BT.601 NV12 并上传，不宣称 GPU 零拷贝。

驱动内部等待在 Windows 默认计时精度下可能多消耗一个刷新量级。编码器活动期间持有配对的 1 ms 计时请求，退出时释放；不会修改系统设置或目标帧率。对支持的驱动申请桌面远程场景和速度优先配置。开发诊断可显式设置 `UU_ENCODE_PROFILE=1`，退出时输出转换、输入提交、驱动完成的聚合耗时；默认关闭，不记录画面。

实现依据微软的 [硬件 MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/hardware-mfts)、[异步 MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts) 和 [H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder) API。Sunshine 与 uu-remote-linux 提供了硬件选择、低延迟参数和恢复行为的参考；本后端未复制其 GPL 代码。

## Linux VA-API

复用已固定的BSD许可cros-codecs/cros-libva API，不依赖外部FFmpeg进程。驱动、上下文和表面由同一线程创建、使用、释放，请求队列有界。选择可用的H.264 Baseline低功耗或常规编码入口，当前目标1–60 FPS。

采集帧仍经过CPU转换与上传。输入使用有限范围BT.601 NV12，编码SPS必须与它一致；支持映射NV12表面的驱动避免额外的图像读回/写回，不支持时使用复制上传。输出为CPU Annex-B码流。这还不是完整GPU管线或零拷贝。

恢复请求需要IDR与SPS/PPS。局部H.264预测器补丁将显式关键帧请求作为新序列起点，避免仅发普通I帧后仍依赖旧参考链。强制恢复帧使用全新软件解码器验证，不能只看持续解码器是否有画面。

## Intel VPL：HEVC、AV1 和 VBR

编码库还提供 Intel VPL 硬件后端，支持驱动实际具备的 H.264、HEVC Main 8-bit 和 AV1 Main 8-bit 编码，以及 CBR / VBR。H.264 CBR 默认继续使用上面的 VA-API / MFT；其它组合由 Intel VPL 执行。在“共享本机”设置编码格式、码率模式和 VBR 峰值；IP、UU 连接服务、自建连接服务共用这些设置。控制端仅在生成测试流实际解码成功后通过 SDP 声明 HEVC / AV1 支持。所选格式不受控制端支持或驱动不满足请求时，连接显示准备失败原因；UU 兼容模式的主机编码选择仍由其服务端决定。

VPL 使用单帧异步深度、无 B 帧、速度优先和有限 VBV；低延迟 BRC 等额外调优由驱动能力决定。初始化后再次读取实际配置，若驱动把帧率、尺寸、编码或码率模式改成不符合请求的值，直接拒绝。仅接受硬件实现，拒绝部分软件加速。恢复关键帧携带实际编码器的参数集，重新设码率后请求新的关键帧；不会伪造参数或把 HEVC / AV1 失败伪装成软件 H.264 成功。

VBR 设置目标和峰值码率，峰值必须不小于目标。CBR 是编码器速率预算，不能保证每一帧、每一秒或加上网络协议后的线上字节数完全恒定。软件 OpenH264 路径保留平均码率控制，不提供这个 VBR 后端。

构建只使用固定版本的 MIT Intel 公共头文件和 C 编译器。运行时需要 Intel GPU 对相应编码的硬件支持及驱动：Linux 安装 Intel VPL dispatcher（`libvpl.so.2`）和 GPU runtime；Windows 正式包随带 MIT 许可的标准 `libvpl.dll` dispatcher，仍需系统 Intel GPU runtime；直接 Cargo 构建时需自行提供 dispatcher。硬件实现由标准 VPL dispatcher 查找，应用不自行扫描驱动目录或直接加载私有实现 DLL。不要求链接 FFmpeg 或启动编码子进程。CPU NV12 转换与上传仍存在，不宣称零拷贝。API 依据 [Intel VPL 低延迟说明](https://intel.github.io/libvpl/latest/appendix/VPL_apnds_c.html) 和 [编码过程](https://intel.github.io/libvpl/v2.10/programming_guide/VPL_prg_encoding.html)，公共头文件来源和固定提交见 `vendor/libvpl-api/UPSTREAM.md`。

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

探针只生成色条，检查范围/矩阵、尺寸裁剪、恢复IDR、码率调整和逐帧解码，不打开屏幕、声音或输入设备。Windows 使用 `--encoder hardware` 要求真正硬编；Linux 可用 `--encoder vaapi`。`--decoder hw` 同时要求硬件解码，不以自动软件回退代替硬件互通验证。使用 `--encoder software` 或 `auto` 检查其它选择；缺少硬件后端的构建保留软件路径。

单台支持VA-API的Linux设备上，720p、1080p、1080×1920竖屏与3840×2160均完成恢复和色彩检查。合成色条的720p调用均值约为软件11.2 ms、复制上传VA-API4.4 ms、映射上传VA-API3.4 ms；4K映射上传约23 ms。这些是编码调用时间，包括转换、上传与等待，不是端到端延迟，也不是帧率保证；4K的该测量尚不足以持续60 FPS。不同码率控制会产生不同的实际字节数，不能将目标码率相同当作输出完全等价。

主工作区测试与实机探针通过。上游依赖的整包开发测试夹具使用过期后端接口，在未修改的固定依赖上同样无法编译；不将其报告为通过，H.264局部补丁以实际恢复探针验证。

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

AMD的硬件MFT编码输出已通过软件解码检查，但当前额外的冷IDR硬件解码检查仍有不出首帧的情况；不将它描述为双硬件互通验收。不同驱动的首次恢复行为仍需按目标硬件验证。

## 原生协议与兼容

默认仍为 H.264，Offer JSON 不增加旧主机无法识别的字段；格式能力放入标准 SDP。H.264 Answer 保持旧字段，非 H.264 连接才使用可选 codec 字段。HEVC 按 RFC 7798 单 NAL / FU 发送，AV1 按 AOMedia RTP 规范聚合和分片；三种格式共用完整访问单元、NACK / RTX、PLI、源时间戳和呈现管线。AV1 重组后恢复明确的 OBU 长度，不把 RTP 聚合头交给解码器。

原生共享界面当前提供 1–60 FPS 和既有 4K60 尺寸范围。编码库更大的格式或帧率参数不代表这条连接已经协商或实际达到相同性能。CLI 三种主机入口均支持 `--codec h264|hevc|av1 --rate-control cbr|vbr --peak-kbps N`；非 H.264 或 VBR 需选择 `--encoder hardware` 或 `auto`。新格式和 VBR 的真实性能还取决于目标硬件与驱动，不以接口模拟测试代替实机验证。
