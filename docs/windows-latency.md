# Windows 视频延迟与计时范围

目标是在保留垂直同步的情况下减少客户端工作量。H.264、1080p、60 FPS 不足以单独决定解码耗时；解码接口、GPU 同步、显存读回、颜色转换和上传路径都影响结果。不能直接把不同客户端同名 HUD 数值当作同一阶段。

## 当前路径

0.8 及此前的图形观看路径：

```text
H.264 → D3D11 硬解 → 同步读回 NV12 → CPU 转 RGBA
      → ColorImage → RGBA 纹理上传 → UI 与视频绘制 → VSync
```

0.9 候选的图形观看路径：

```text
H.264 → D3D11 硬解 → 同步读回 NV12 → 最新帧邮箱
      → Y/UV 平面上传 → GPU 颜色转换与缩放 → UI 绘制 → VSync
```

两者都保留硬件解码和默认垂直同步。平面路径去掉 CPU YUV 转换及逐像素 ColorImage 创建，1080p 每帧上传量从约 8.3 MB 降到约 3.1 MB。纹理存储按格式与尺寸复用。Windows 读回存储在最后一个 CPU 图像读者释放后归还到有界缓冲池，避免逐帧分配与清零；不外借映射指针。帧邮箱每个显示器仅保留最新图像，不增加显示帧队列；解码参考帧仍按序处理。

着色器支持 NV12、I420、BGRA，以及 BT.601/709 的完整和有限范围。初始化失败使用原 RGBA 回退。CLI 的 RGBA 消费接口保持兼容。

**这里仍存在 D3D11 同步读回，尚不是零拷贝。** 真正去除该步骤需要让解码表面直接进入兼容的 GPU 渲染路径，并处理设备共享、同步、显示器切换和资源生命周期。不能通过关闭防重放、丢弃 H.264 参考帧或关闭 VSync 来代替。

## HUD

- Decode 是解码调用耗时，Windows 当前包括 MFT 处理、GPU 完成等待及读回，不包括之后的 CPU 颜色转换。
- Render 是 eframe 一轮 UI 准备和绘制提交的 CPU 墙钟时间，包括可能阻塞的驱动调用；交换缓冲区的显式 VSync 等待已排除。它不是 GPU 时间戳测量，也不是端到端显示延迟。
- FPS、带宽、解码均值及 Render 使用约 500ms 的采样窗口；HUD 以 500ms 快照显示并缓存排版。视频和输入继续按原有节奏处理，统计采样不延迟帧。
- 显示器、分辨率、DPI 或布局宽度改变时，HUD 立即重新排版。

关闭 VSync 后 Render 仍可能下降，因为驱动排队与纹理资源的同步等待也会影响绘制提交。应分别检查上传和同步，不能据此认为必须关闭 VSync。

## 隔离验证

`video_renderer_probe` 仅绘制生成的像素并读取自己的 GL 帧缓冲。Linux 必须通过 `tools/native/input_smoke.py` 启动私有认证 Xvfb；Windows 必须显式指定 `--isolated-window-station`，创建全新的非交互窗口站。它不读取工作桌面、不发送全局输入或访问剪贴板。

默认测试颜色、方向、行填充、透明字节处理、格式/尺寸变化及复用；`--benchmark` 测试 1080p NV12 上传，`--rgba-reference` 使用原 RGBA 路径作为对照。结果中的 `pixel_pipeline_cpu_ms` 包括测试程序的像素准备和绘制提交；`ui_and_submit_ms` 扣除单独计时的 CPU 颜色转换。测试窗口缩放显示，因此这些数字不是全屏扫描或用户实际串流 HUD 的替代。

`UU_DECODE_PROFILE=1` 启用 Windows 解码元数据计时，按 120 次调用输出整体、同步读回、CPU 拷贝和剩余 MFT 工作。它不会保存图像；默认关闭。首次初始化应与后续稳定窗口分开比较。

## 参考

- [Microsoft：H.264 解码器低延迟接口](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avlowlatencymode)：内置 H.264 实现要求 `VT_UI4`。属性接口不可用时通过 `ICodecAPI` 设置；不支持该提示仍保留可用硬解。
- [Moonlight Qt](https://github.com/moonlight-stream/moonlight-qt)：参考其硬件表面直接渲染、解码计时、显示排队分离的架构思路。本项目的平面着色器与邮箱实现独立编写，未引入其 GPL 渲染源文件。
