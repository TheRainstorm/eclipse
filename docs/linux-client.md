# Linux 客户端与自动验收

主程序可直接检出构建，无需下载插件源码。可选插件单独下载、构建和发布，见[插件说明](plugins.md)。

Linux VA-API 支持完整访问单元提交，并依据 H.264 色彩标记选择范围和 BT.601 / BT.709 转换。

开发分支的“应用设置 → 串流 → 视频编码”可选择优先 H.265 / HEVC。Linux 通过 VA-API 解码 HEVC Main 8 位视频；连接前先用生成流验证实际解码输出，成功后才向 UU 被控端声明支持，失败时使用 H.264。需要启用 `vaapi` 构建和支持 HEVC Main 的显卡驱动；强制软件解码时不会声明 HEVC，不能把 OpenH264 当作 HEVC 回退。

硬件环境可显式运行 `cargo run --release -p uu-media --example hevc_decode_probe --features vaapi --locked` 验证生成流，增加 `-- --leases` 检查 GPU 表面租约。探针不读取桌面，不保存画面。编译成功不代表显卡驱动验收成功。

2026-10-19 在 Linux 测试客户端（Intel Tiger Lake、Arch、KDE Wayland）连接 Windows 测试主机 实测。当前目标是控制端；`uu probe` 中 Linux **被控端** uinput 检测失败不影响控制 Windows 主机。

## 构建和统一安装

开发机需要 Rust、C/C++、Clang、nasm、pkg-config，以及 libva/libgbm/libdrm/ALSA/PulseAudio、PipeWire 和 XKB 的开发包。Ubuntu 可安装 `libva-dev libgbm-dev libdrm-dev libasound2-dev libpulse-dev libpipewire-0.3-dev`，Arch 使用对应 `libva mesa libdrm alsa-lib libpulse pipewire libxkbcommon libxkbcommon-x11` 软件包。Ubuntu 的 X11 支持还需 `libxkbcommon-x11-dev`（运行时为 `libxkbcommon-x11-0`）；该库在启动 X11 后端时动态加载，单独运行 `ldd` 无法检查它是否存在。运行桌面推荐安装 PipeWire ALSA 插件和中文字体。

```sh
cargo run -p uu-gui --features vaapi
python3 tools/linux/package.py
# 到生成的 artifacts/packages/eclipse-linux-x86_64-<revision>/ 内：
python3 install.py
```

同一提交已有完整安装包时，打包脚本核验内容、目标平台、功能和 glibc 兼容性后直接复用，不调用 Cargo；解包目录缺失时从已核验归档恢复。`--rebuild` 可强制执行构建流程，仍使用 Cargo 缓存。检查与测试产物不等于 release 安装包，因此首次打包仍需要 release 构建。

正式入口为 `~/.local/bin/eclipse`、`~/.local/bin/eclipse-cli` 或应用菜单 **Eclipse**。实际文件在 `~/.local/opt/eclipse/releases/<revision>/`，`current` 指向当前版本。旧 `uu-client`、`uu-remote-rust` 和 `uu-rust` 命令保留为兼容别名。升级保留原用户配置和旧版本文件；请退出旧窗口后重新启动。

应用菜单使用与窗口 app_id 一致的 `Eclipse.desktop`。旧桌面启动项保留为隐藏兼容入口；需要更新固定图标时，从应用菜单重新固定 Eclipse。安装路径中的空格和特殊字符按桌面启动项规则转义，启动不经过 shell。

## Wayland 本机共享

在“共享本机”开启服务后，远端通过设备认证并请求画面时，由系统弹窗选择共享屏幕。Wayland 不预先读取屏幕内容或用 Xwayland 根窗口替代桌面；屏幕尺寸以系统授权后的实际画面为准，现阶段不保存门户的长期恢复令牌。

允许控制时，画面和键鼠通过同一个 RemoteDesktop 门户会话获得授权。如果桌面只提供 ScreenCast，Sway 等 wlroots 合成器可在屏幕授权后使用同一合成器的 virtual-keyboard / virtual-pointer，绑定获选输出并处理缩放；虚拟输入协议不可用时继续只观看。RemoteDesktop 明确拒绝输入授权时不改走虚拟输入。屏幕选择不请求持久化权限；组合会话的持久化仅能由 RemoteDesktop 管理，本实现不保存恢复令牌。鼠标绝对位置按屏幕像素与门户逻辑尺寸换算，适配缩放；停止控制仅释放当前会话按下的键和按钮。取消系统弹窗、断线及停止共享会关闭相应会话。Wayland 文本和图片剪贴板仍取决于独立后端是否可用。客户端输出帧率可动态调整；可变帧率源保留门户本身的捕获上限，不保证减少到同样的捕获工作量。部分 wlr 后端在活动中重协商源格式会破坏在途捕获对象，因此当前保持一次授权内的捕获格式稳定，由编码线程控制输出节奏。源被移除或连接失败时停止使用缓存画面并结束会话。

需要运行 PipeWire、`xdg-desktop-portal` 和与桌面匹配的后端，例如 KDE 的 `xdg-desktop-portal-kde` 或 GNOME 的 `xdg-desktop-portal-gnome`。门户 API 定义见 [ScreenCast](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html) 和 [RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)。系统拒绝授权时显示失败原因，不能通过应用绕过授权。

开发验证可使用独立 D-Bus 与生成视频，不接触真实桌面和系统输入。需要 `python3-dbus python3-gi`，生成视频还需 PipeWire 工具及 SPA 视频测试源：

```sh
cargo build --release -p uu-capture --example portal_probe --locked
for scenario in deny cancel stream-view stream-input stream-no-input stream-keyboard-only stream-input-denied; do
  python3 tools/native/portal_smoke.py --probe target/release/examples/portal_probe --case "$scenario"
done
python3 tools/native/portal_smoke.py --probe target/release/examples/portal_probe \
  --cli target/release/eclipse-cli --case stream-native
```

这覆盖真实门户调用、拒绝/取消后关闭、PipeWire 帧传输、同进程重连、缩放坐标、按键释放及生成画面的原生加密串流；不代表各桌面的授权弹窗交互均已实机验收。

真实 wlr 后端可在独立 headless Sway、D-Bus 和 PipeWire 中验证。需要 Sway、foot、PipeWire 工具、GDBus 与 wlr 门户后端；不同发行版可用 `--portal` / `--backend` 指定系统二进制位置。下面只向该夹具的第二个缩放输出发送已知测试按键和点击，不连接用户桌面：

```sh
cargo build --release -p uu-core --example native_startup_probe --features vaapi --locked
python3 tools/native/wayland_portal_startup.py \
  --probe target/release/examples/native_startup_probe \
  --output artifacts/real-portal-check --frames-per-session 180 \
  --exercise-quality --exercise-input
```

夹具检查30→60→24输出帧率切换、持续解码、再次连接，以及私有终端收到键盘和中心点击。另用 `--sessions 1 --terminate-backend-after 1` 验证捕获源结束后及时报错，防止把缓存帧计数误判为仍在捕获。`--frames-per-session` 可增大为有界长测；所有统计仍不等同于用户桌面的端到端延迟。

## 独立 Wayland 测试桌面

需要 `sway grim python3` 和 VAAPI 驱动；没有 GPU 时可用 `--renderer pixman` 测功能。测试依靠两个显示源的固定 KVM 测试机，输入/鼠标布局为 1600×1000 私有输出。不会向用户 KDE 会话注入输入。

```sh
cargo build --release -p uu-gui -p uu-cli --features vaapi --locked
cargo build -p uu-gui --example wayland_input --locked
cargo build --release -p uu-gui --example clipboard_probe --locked
python3 tools/linux/wayland_smoke.py \
  --binary target/release/eclipse \
  --input-driver target/debug/examples/wayland_input \
  --clipboard-probe target/release/examples/clipboard_probe \
  --state "$HOME/.local/share/uu-remote/state.json" \
  --device DEVICE_ID --output /tmp/uu-wayland-run-01
```

输出目录必须是新的。脚本创建独立 Sway 和标准 US 物理键盘布局的虚拟键鼠，检查真实首帧/解码器、HUD 开关、X/F 全屏、系统快捷键捕获和释放、四次显示源切换、Q 正常退出。保留截图、元数据和结果 JSON；结束删除临时凭据副本、运行目录及自己启动的进程。

这里不能用临时 wtype 进程替代完整驱动：最后一台虚拟键盘移除会产生人为失焦；动态符号键盘的物理键码也不等于真实 US 键位，可能让本应输入字母的测试发送数字。鼠标同样需要真正的虚拟 pointer，单纯 Sway IPC cursor 命令不能证明客户端收到了输入。

`clipboard_probe` 在该独立剪贴板验证中英文标点、双向文本、回环抑制、只读和失焦停用。不读取用户 KDE 剪贴板。

远端键盘验收可在专用测试主机的**交互桌面**运行 `tools/windows/keyboard-fixture.ps1`，然后向上述脚本增加 `--keyboard-fixture`。增加 `--relative-mouse` 可验证Locked相对模式；`--soak-seconds 60` 延长运行。F22 只激活测试文本框；脚本输入固定文本 `aA;:,.?/[]{}+-_` 并发送 Ctrl+C/V/X。文本框禁用自己的 IME、拦截复制/粘贴/剪切，保留系统剪贴板；45 秒后关闭并恢复原前台。通过指定 `-Output` 读取 JSON，逐字检查结果，不能仅以“已发送”当作输入通过。不要在没有该专用文本框时启用此参数。

## 确认硬解与定位延迟

串流时按 Ctrl+Alt+Shift+S 开启 HUD，悬停诊断可查看实际使用的 `VAAPI 硬解` 或 `OpenH264 软解` 后端和回退原因。编译时启用 `vaapi` 或 `vainfo` 显示支持 H.264，都不能单独证明当前会话正在硬解。

Decode 包含解码调用、线程往返和硬件帧下载，并非只有视频芯片运行时间；De 是解码输出帧率，Rd 是提交的新画面帧率，两者可能不同。新版对线性与普通 Intel Y 平铺 NV12 表面使用只读 DMA-BUF 下载，避免 VAImage 映射时的双向平铺转换，并复用 CPU 输出缓冲。仍然存在 CPU 下载和纹理上传，不是 GPU 零拷贝；未知或压缩布局自动保留驱动下载路径。

开发分支包含实验性的 EGL 导入路径，可用 `UU_GPU_VIDEO=1 uu-client` 显式启用，把已同步的 VAAPI NV12 DMA-BUF 直接绑定为纹理，省去 CPU 下载与重新上传。该路径的分辨率变化和完整性能验收尚未完成，候选包默认保留已验证的 CPU 下载/平面上传。当前支持线性和普通 Intel Y 布局；使用原有着色器处理色彩与可见区域裁剪。解码池的底层缓冲会保留到 GPU 采样完成，通过非阻塞 fence 检查回收；保留垂直同步，不新增待显示视频队列。能力不足、GLX 上下文、未知布局或导入失败时保留 CPU 下载/平面上传路径。可使用 `UU_GPU_VIDEO=0 uu-client` 明确选择基线做同机对照；诊断中的 `video_render_path` 区分 `dma-buf-egl` 和 CPU 上传。硬解 CLI 默认仍返回 CPU 像素。

EGL 导入依据 [Khronos DMA-BUF 扩展](https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import.txt)和[格式修饰符扩展](https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import_modifiers.txt)。CI 构建 `linux-gpu-validation` 探针，实机需在独立的测试合成器上设置 `UU_GPU_VIDEO=1` 后运行；它只读取生成视频在自身 framebuffer 中的像素，逐帧对比 GPU 导入与 CPU 上传，不能把构建成功当作实机验收。

开发者可用生成的 H.264 文件进行同机对照：

```sh
UU_DECODE_PROFILE=1 uu-rust probe --h264 generated.h264
UU_DECODE_PROFILE=1 UU_VAAPI_READBACK=legacy uu-rust probe --h264 generated.h264
```

每 120 次调用输出一组不含视频内容的分段均值。离线解码吞吐不能替代真实 GUI 的流畅度测量。会话诊断同时记录分辨率、音频队列、欠载和丢弃计数，用于区分画面提交不足与音频调度问题。

为避免用 720p 的结果推断 1080p 表现，原生生成源可显式设置 `native-host --synthetic --synthetic-width 1920 --synthetic-height 1080`，同时按原生直连文档指定私有邀请文件与监听地址。此选项只改变测试图案，不修改真实显示器。

## 音频与硬解验收

```sh
cargo build --release -p uu-media --example audio_probe --locked
python3 tools/linux/audio_smoke.py \
  --probe target/release/examples/audio_probe --output /tmp/uu-audio-run-01
UU_DECODER=hw target/release/eclipse-cli probe --h264 TEST.h264
```

音频测试需要 PipeWire/Pulse 兼容服务、`pactl`、ffmpeg Pulse 输入和 ALSA PipeWire 插件。只创建自己的空声卡，使用进程专用 ALSA 配置，不修改默认播放设备；录制其 monitor 并检查 440Hz、RMS 和播放状态，最后卸载自己的模块。用的是实际 Opus 解码、PCM 队列和 CPAL 输出路径。

硬解必须检查最终仍为 `vaapi`、无错误和无线程崩溃；“初始化成功”不足以证明硬解可用。当前采用驱动分配的 DMA surfaces 和 VAImage 映射，避开桌面 Mesa 不支持的 ChromeOS GBM 标志与上游 DMA 去平铺越界。新版完整AU提交后，无重排120 AU样本输出120帧、首帧由AU2提前到AU0；含B帧样本仍有尾帧需要flush。不能把尾差当网络丢包，也不能据离线吞吐声称实机持续60/120FPS。

实测多轮完整 Wayland 套件及正式安装包相对模式60秒持续运行均退出码0，四次切屏均完成、无切屏超时，元数据保持 AMD vDisplay 和 Zako HDR 两个物理源；键盘固定文本逐字匹配、C/V/X 均确认。Opus 空声卡录到440Hz测试音。正式包60秒运动画面测试末帧HUD约57 Rx / 56.7 De / 59.2 Rd FPS，解码4.60ms、解码错误0、坏帧0；只作为该次短测快照。仍需日常长稳、相对鼠标游戏手感、真实多屏跨DPI和其它GPU回归。

### PyroWave 运行库

v0.19.0 完整包增加 `libpyrowave-shared.so.1`。源码打包首次需要 Git、CMake 3.20+ 与 C++14 编译器以构建固定上游，之后核验散列并复用缓存。启用此格式还需 Vulkan 驱动；当前 Linux 路径包含像素上传/读回，范围、带宽与测量见 [PyroWave](pyrowave.md)。默认编码不变。
