# HDR 串流

Eclipse 原生串流可以把 Windows 主机的 HDR10 桌面送到 Windows 或 Linux Wayland 客户端。请更新两端程序和主机后台服务。UU 兼容连接使用 SDR。

1. 在被控 Windows 主机上，为**要串流的那块显示器**打开 HDR。虚拟显示器也需要启用 HDR。
2. 在客户端窗口所在的显示器上打开 HDR。Linux 客户端需要使用带 VA-API 的构建，显卡能硬解所选的 10 位格式，Wayland 合成器能为该窗口提供 BT.2020/PQ 输出、十位 DMA-BUF 呈现和逐帧回执。X11 和纯软件解码使用 SDR。
3. 在客户端「画面」中开启「优先 HDR」（默认开启），选择自动、HEVC 或 AV1，然后重新连接。主机根据所选显示器、硬件编码器和客户端的实际解码能力协商。H.264 使用 SDR。

画面菜单和 HUD 显示本次连接与当前窗口的结果。

| 显示状态 | 含义 |
| --- | --- |
| HDR10 | 双方已协商 HDR10，当前窗口已确认使用 HDR 输出。 |
| SDR | 当前传输 SDR。Linux 客户端在 SDR 屏上发起连接时，即使已开启「优先 HDR」，也会选用 SDR。要切回 HDR，请把窗口移到已启用 HDR 的屏幕并重新连接。 |
| HDR10 → SDR | 仍在接收 HDR10，窗口正在将画面映射到 SDR。例如已协商 HDR10 后把窗口移到 SDR 屏。 |
| — | 正在连接，或当前 HDR 窗口的呈现状态尚未确认。 |

Windows AMD 编码使用原生 AMF 的 HEVC Main10 或 AV1 10 位模式；其它 Windows 显卡会尝试支持 P010 的硬件 MFT，按实际能力决定是否使用 HDR。当前 HDR 采集支持未旋转的整屏输出。关闭 HDR、旧客户端或本次编码不支持 HDR 时，主机使用平台的 SDR 采集路径。已请求 HDR 采集、但 HDR 编码初始化失败时才对采集的 HDR 桌面应用 SDR 映射；软件或只能接受 CPU 像素的编码器需要 CPU 处理，建议使用自动硬件编码以减少延迟。

## 色彩与数据路径

采集格式遵循本次客户端请求、活动 RTP profile 与候选编码的共同能力；普通 SDR 会话不额外经过 scRGB 映射。HDR 会话的 Windows Desktop Duplication 使用 `DuplicateOutput1` 保留浮点 scRGB 桌面，GPU 将其转换为 BT.2020、ST 2084（PQ）的 P010。10 位样本在 16 位字中左对齐，使用有限范围的亮度和色度编码。源为 SDR BGRA 时，先还原其传递函数并按 SDR 白色亮度转换。

编码器必须产生实际的 10 位 PQ/BT.2020 码流，关键帧头会再次校验，不能只修改编码标签。原生会话使用独立的可选 SDP 属性传递 HDR 能力和显示信息，HEVC Main10 使用独立的 RTP profile；旧版信令和普通 SDR 设置保持兼容。

Windows 客户端将 P010 硬解纹理转为浮点线性 RGB，合成视频、光标与菜单，再通过 scRGB 交换链呈现。Linux Wayland 客户端在支持的设备上通过 EGL 导入 P010 双平面，在浮点场景内合成视频和界面，再写入标记为 BT.2020/PQ 的十位 Wayland 子表面。窗口确认该子表面已在当前 HDR 输出呈现后，才在 HUD 显示 HDR10。窗口回到 SDR 输出时使用普通 SDR 呈现。

Windows SDR 硬解后的 NV12 使用显式着色器转换为 RGB，按 BT.601/BT.709、全范围/有限范围和左对齐色度采样规则重建，并限制采样到实际可见区域，避免读取编码对齐填充。缺少所需着色器或平面纹理能力的旧设备保留视频处理器回退。诊断中的 `video_color` 会记录实际格式、色彩解释及转换后端。

Windows 硬编的 SDR GPU 输入也显式转换为 NV12：先按输出分辨率采样 RGB，再以水平方向的三点滤波与垂直方向的两点平均生成左对齐色度，减少文字子像素边缘在降采样时产生的彩色走样。普通 BGRA 保留原有 SDR 信号，浮点 scRGB 的 SDR 回退先应用原亮度映射。HDR P010 同样使用左对齐色度，在转为 PQ 前先在线性 RGB 中滤波；GPU 解码和 P010 平面渲染按相同采样位置重建，并排除编码填充区域。转换和缩放均在 GPU 完成，平面渲染能力不足时使用原视频处理器。4:2:0 仍会损失细小真彩色细节，不能等同于 4:4:4 或 RGB 无损传输。

scRGB 的 `1.0` 对应 80 cd/m²，PQ 表示绝对亮度；Windows 界面白色使用系统当前 SDR 白色亮度，Linux Wayland 的 HDR 界面参考白为 203 cd/m²。实现参考 [Windows Advanced Color 文档](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range)、[Sunshine](https://github.com/LizardByte/Sunshine) 和 [Moonlight 的 D3D11 呈现设计](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/streaming/video/ffmpeg-renderers/d3d11va.cpp)。

## 验证边界

生成的 HDR 图案可检查亮度、色彩矩阵、位深、硬编硬解和呈现表面；这些检查不能替代显示器上的视觉验收。HDR 效果还取决于显示器、Windows HDR 校准、驱动和内容本身。实际桌面延迟应在使用中的分辨率、帧率和内容下测量。

`tools/native/hdr_color_probe.cpp` 使用生成的浮点细边缘，对生产 GPU 转换结果与独立 CPU PQ、滤波和采样参考逐像素比较，不采集桌面。在 Windows MinGW 环境可运行：

```sh
g++ -std=c++17 -O2 tools/native/hdr_color_probe.cpp -o hdr_color_probe.exe -ld3d11 -ld3dcompiler -ldxgi -static-libgcc -static-libstdc++
./hdr_color_probe.exe
```
