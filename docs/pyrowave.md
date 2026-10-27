# PyroWave 原生串流

v0.19.0 增加实验性 PyroWave 支持。两端使用同版 Eclipse，在主机的“允许的编码”中启用 PyroWave，再由客户端选择“PyroWave（实验性 / SDR）”。切换编码需要重新连接；默认自动协商仍按 AV1 → HEVC → H.264，UU 兼容连接不支持此格式。

当前支持 SDR、8 位、BT.709 全范围、4:2:0 与 CBR 帧预算。暂不支持该格式的 HDR、4:4:4、VBR 或 Linux DMA-BUF 直接导入。选择不可用的格式会返回错误，不会悄悄替换成 H.264。软件解码选项不能用于 PyroWave。AMD / Intel / NVIDIA 的传统硬件编码调优只作用于对应后端，不限制 PyroWave，也不会因切换此格式改写主机的这些设置。

PyroWave 以较高带宽换取快速的 GPU 编解码。“推荐码率（LAN Quality）”采用上游良好画质经验曲线，1080p60 约 170 Mbps、4K60 约 288 Mbps，帧率翻倍时码率约翻倍；也可以手动调整。完整表格、计算规则和适用范围见 [LAN Quality 推荐码率](bitrate-recommendations.md)。实际网络还要承载 RTP、SRTP 与修复数据，因此不能把目标码率等同于网卡所需带宽，推荐有线网络。每帧独立编码，图像复杂度和码率仍影响画质；一次生成图检查不能代替实际桌面文字、游戏或运动素材验收。

## Rust 与 C++ 的边界

主程序继续使用 Rust：会话、设置、协商、输入、音频、网络与帧队列沿用现有实现。通过上游已有的 C API 调用 C++ Vulkan 运行库，桥接只传固定布局参数、字节切片和不透明句柄，不传 C++ 对象。运行库随完整安装包提供，只从安装目录加载，并核对精确 C API 版本。

Windows 编码按采集设备的完整 LUID 选择同一块 Vulkan GPU。画面通过 GPU 拷贝进入可共享的 BGRA 纹理，再由 Vulkan 缩放、转换与编码；CPU 只读取压缩码流供网络发送。D3D11 fence 与 Vulkan timeline semaphore 交还纹理所有权，采集租约保持到输入读完。

Windows 解码将 Y/U/V 写入共享 GPU 平面，再由 D3D11 转换成带租约的 BGRA 输出，接入原有 WGL 显示。输出池最多四槽；渲染器仍持有某帧时，该槽不能被覆盖。池满时丢弃新帧，释放后重用；帧还可在解码器句柄释放后保留其会话和设备。正常显示路径不读回完整图像。

Windows 能力探测验证实际使用的 D3D11 NT 纹理、D3D fence 导入和 GPU 解码，不要求驱动支持未使用的旧式 KMT 句柄。驱动可以不支持后者；其缺失不代表上述串流路径不可用。参见 [Vulkan Windows 外部显存规范](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_external_memory_win32.html)。

Linux 当前通过 CPU 上传、GPU 编解码、CPU 读回接入，后端明确显示为 `pyrowave-vulkan-cpu-upload` / `pyrowave-vulkan-cpu-readback`。它可以互通与验证格式，不能据此宣称实现了 GPU 直接传输或亚毫秒完整管线。

新格式使用独立 SDP 选择属性和固定的 RTP 格式契约，保留旧版严格解析的画质请求。沿用原生连接的加密、重排、丢包修复和资源上限；RTP 分片携带完整帧长度及偏移。解码前检查图像尺寸、块索引、序号、重复块、位平面和符号数据长度。旧客户端未提出该格式时不会收到它。

Windows 需要在已登录的交互会话中启动 GPU 串流。在本次测试的系统上，SSH Session 0 的 D3D11 和 Vulkan LUID 不一致，匹配被拒绝；同一程序在已登录会话中正常。不能以厂商相同代替适配器身份匹配，也不修改显示或虚拟机配置来绕过。

## 本次测量与限制

RX 7900 XTX、Windows、Release 构建；每种分辨率使用同一张生成渐变/条纹图，预热 3 帧后取 30 帧，60 FPS 预算。完整编码调用包含输入共享、缩放、GPU 编码、码流读回与打包；解码调用是 CPU 校验和 GPU 提交，尚未等待显示完成。

| 条件 | 编码调用 p50 / p95 | 解码提交 p50 / p95 | 解码 GPU 阶段均值合计 | 平均 RGB 误差 |
|---|---:|---:|---:|---:|
| 1920×1080、200 Mbps | 0.741 / 0.892 ms | 0.745 / 0.884 ms | 约 0.070 ms | 0.98 / 255 |
| 3840×2160、800 Mbps | 1.700 / 1.862 ms | 2.395 / 2.530 ms | 约 0.201 ms | 0.98 / 255 |

编码 GPU 各阶段均值合计约 0.156 / 0.388 ms，缩放另约 0.023 / 0.089 ms。上游 GPU 内核示例与这里的完整调用不是同一口径，也不是同一款显卡。本次未测量网络、WGL 呈现、物理屏幕扫描或输入到光子的延迟，不能把上述阶段相加当作端到端测量。

Linux GTX 1070 的 CPU 传输路径，同一 1080p 图在 200 Mbps 下编码调用 p50 约 9.81 ms、解码调用 p50 约 1.76 ms。该路径包含 CPU 色彩转换和像素传输，GPU 时间较短不能掩盖这些开销。

原生加密 localhost 生成视频已验证 60 → 120 → 60 FPS 及码率切换，持续输出 32 帧、解码错误 0；这属于协议及管线验证，实际双机桌面和用户显示环境仍需验收。

## 构建与复测

完整 Windows / Linux 打包入口自动构建并收录运行库。依赖首次从固定的公开 Git 提交获取，后续复用通过散列检查的缓存。单独构建及生成图测试：

```sh
python3 tools/pyrowave/build.py --jobs 6
cargo build --release -p uu-media --example pyrowave_probe
# 将 RUNTIME 替换为构建输出的运行库路径；Windows 使用对应 .dll。
cargo run --release -p uu-media --example pyrowave_probe -- RUNTIME 1920 1080 30 200000
# Windows GPU 输入、GPU 输出及槽位生命周期检查：
cargo run --release -p uu-media --example pyrowave_probe -- RUNTIME 1920 1080 30 200000 --gpu --lease-check
```

探针只使用自行生成的图像，不采集桌面、不注入输入、不修改显示或服务。JSON 分别记录 CPU 调用、上游 GPU timestamp、实际码率和像素误差；Windows 解码提交不会被标为 GPU 完成耗时。非交互 SSH 进程的适配器可能不同，测试必须使用实际串流的登录会话。

实现固定到 [PyroWave d7a03df](https://github.com/Themaister/pyrowave/tree/d7a03df74bff7f54724be3850ab4b2a673e1bede)，接口见[上游 C API](https://github.com/Themaister/pyrowave/blob/d7a03df74bff7f54724be3850ab4b2a673e1bede/pyrowave.h)，码流语义见[上游规范](https://github.com/Themaister/pyrowave/blob/d7a03df74bff7f54724be3850ab4b2a673e1bede/bitstream/bitstream.md)。仅使用上游 MIT 实现及所需依赖，随包保留许可；不依赖其它项目私有扩展的 ABI。
