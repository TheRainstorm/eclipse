# Linux 客户端与自动验收

2026-10-19 在 laptop-arch（Intel Tiger Lake、Arch、KDE Wayland）连接 KVM-WIN11 实测。当前目标是控制端；`uu probe` 中 Linux **被控端** uinput 检测失败不影响控制 Windows 主机。

## 构建和统一安装

开发机需要 Rust、C/C++、Clang、nasm、pkg-config，以及 libva/libgbm/libdrm/ALSA 的开发包。Ubuntu 可安装 `libva-dev libgbm-dev libdrm-dev libasound2-dev`，Arch 使用对应 `libva mesa libdrm alsa-lib` 软件包。运行桌面推荐安装 PipeWire ALSA 插件和中文字体。

```sh
cargo run -p uu-gui --features vaapi
python3 tools/linux/package.py
# 到生成的 artifacts/packages/uu-remote-linux-x86_64-<revision>/ 内：
python3 install.py
```

正式入口：`~/.local/bin/uu-remote-rust`，或应用菜单 **UU Remote Rust**。实际文件在 `~/.local/opt/uu-remote-rust/releases/<revision>/`，`current` 为统一版本入口。CLI 是 `~/.local/bin/uu-rust`。安装器校验二进制 SHA256 和动态依赖，原子切换入口，旧版本保留。版本显示读取同目录 `version.json`；不要单独复制 exe/二进制而遗漏清单。

凭据和设置在 `$XDG_DATA_HOME/uu-remote`（默认 `~/.local/share/uu-remote`）。安装包不含凭据，首次运行可扫码登录。安装升级不覆盖设置或凭据。

默认保持远端显示器布局；独占虚拟屏只在用户显式选择时启用。支持绝对桌面鼠标、Locked 相对游戏鼠标、本地/远端光标偏好。窗口按目标屏幕约 80% 容量计算大小；Wayland 的位置由合成器决定，应用不能强制全局居中坐标。

快捷键：Ctrl+Alt+S 信息，F/X 串流全屏，Q 退出串流，M 菜单，Z 释放/捕获。Wayland 全屏系统快捷键使用 `keyboard-shortcuts-inhibit`，是否允许由合成器决定。退出全屏/失焦时释放。窗口模式将 Super 留给本地桌面。X11 普通键盘、鼠标、文本剪贴板可用；X11 的全屏系统键抓取尚未实机闭环。

## 独立 Wayland 测试桌面

需要 `sway grim python3` 和 VAAPI 驱动；没有 GPU 时可用 `--renderer pixman` 测功能。测试依靠两个显示源的固定 KVM 测试机，输入/鼠标布局为 1600×1000 私有输出。不会向用户 KDE 会话注入输入。

```sh
cargo build --release -p uu-gui -p uu-cli --features vaapi --locked
cargo build -p uu-gui --example wayland_input --locked
cargo build --release -p uu-gui --example clipboard_probe --locked
python3 tools/linux/wayland_smoke.py \
  --binary target/release/uu-gui \
  --input-driver target/debug/examples/wayland_input \
  --clipboard-probe target/release/examples/clipboard_probe \
  --state "$HOME/.local/share/uu-remote/state.json" \
  --device DEVICE_ID --output /tmp/uu-wayland-run-01
```

输出目录必须是新的。脚本创建独立 Sway 和标准 US 物理键盘布局的虚拟键鼠，检查真实首帧/解码器、HUD 开关、X/F 全屏、系统快捷键捕获和释放、四次显示源切换、Q 正常退出。保留截图、元数据和结果 JSON；结束删除临时凭据副本、运行目录及自己启动的进程。

这里不能用临时 wtype 进程替代完整驱动：最后一台虚拟键盘移除会产生人为失焦；动态符号键盘的物理键码也不等于真实 US 键位，可能让本应输入字母的测试发送数字。鼠标同样需要真正的虚拟 pointer，单纯 Sway IPC cursor 命令不能证明客户端收到了输入。

`clipboard_probe` 在该独立剪贴板验证中英文标点、双向文本、回环抑制、只读和失焦停用。不读取用户 KDE 剪贴板。

远端键盘验收可在专用测试主机的**交互桌面**运行 `tools/windows/keyboard-fixture.ps1`，然后向上述脚本增加 `--keyboard-fixture`。F22 只激活测试文本框；脚本输入固定文本 `aA;:,.?/[]{}+-_` 并发送 Ctrl+C/V/X。文本框禁用自己的 IME、拦截复制/粘贴/剪切，保留系统剪贴板；45 秒后关闭并恢复原前台。通过指定 `-Output` 读取 JSON，逐字检查结果，不能仅以“已发送”当作输入通过。不要在没有该专用文本框时启用此参数。

## 音频与硬解验收

```sh
cargo build --release -p uu-media --example audio_probe --locked
python3 tools/linux/audio_smoke.py \
  --probe target/release/examples/audio_probe --output /tmp/uu-audio-run-01
UU_DECODER=hw target/release/uu probe --h264 TEST.h264
```

音频测试需要 PipeWire/Pulse 兼容服务、`pactl`、ffmpeg Pulse 输入和 ALSA PipeWire 插件。只创建自己的空声卡，使用进程专用 ALSA 配置，不修改默认播放设备；录制其 monitor 并检查 440Hz、RMS 和播放状态，最后卸载自己的模块。用的是实际 Opus 解码、PCM 队列和 CPAL 输出路径。

硬解必须检查最终仍为 `vaapi`、无错误和无线程崩溃；“初始化成功”不足以证明硬解可用。当前采用驱动分配的 DMA surfaces 和 VAImage 映射，避开桌面 Mesa 不支持的 ChromeOS GBM 标志与上游 DMA 去平铺越界。120 AU 的离线样本输出118帧，当前 stateless 后端存在内部尾帧/提交等待；不能把这个尾差当网络丢包，也不能据离线吞吐声称实机持续60/120FPS。

实测两次完整 Wayland 套件退出码0，四次切屏均完成、无切屏超时，元数据保持 AMD vDisplay 和 Zako HDR 两个物理源；键盘固定文本逐字匹配、C/V/X 均确认。Opus 空声卡录到440Hz测试音。仍需日常长稳、相对鼠标游戏手感、真实多屏跨DPI和其它GPU回归。
