# uu-client

> **版权与项目声明**：本项目由个人维护，目前无盈利，与网易及其 UU 远程产品无隶属、合作或官方授权关系。第三方名称仅用于说明兼容对象，相关商标与权利归原权利人所有。如 UU 官方或相关权利人要求下架，维护者将配合停止公开分发并处理相关内容。独立实现、非盈利及此声明不代表已获得接入许可，也不消除服务协议或知识产权风险；使用前请阅读[许可与兼容接入说明](docs/licensing-and-compatibility.md)。

<p align="center">
  <img src="assets/branding/eclipse-256.png" width="88" height="88" alt="清新日蚀图标">
</p>
<p align="center"><strong>远端，如在眼前。</strong><br>面向 Windows 与 Linux 的原生远程串流客户端</p>

uu-client 使用 Rust 构建，通过兼容协议连接 UU 远程被控端，提供设备列表、远程画面、音频、键鼠和多显示器操作。Linux 客户端原生运行，无需 Wine。

目前已达到日常远程桌面基本可用的 demo 阶段，仍在快速迭代。另有[实验性原生局域网串流通路](docs/native-streaming.md)，可使用自有主机、配对与加密视频传输；已提供图形端共享、显式授权的键鼠控制、[系统声音与音频恢复](docs/native-audio.md)，以及[双方许可文本剪贴板](docs/native-clipboard.md)，不需要 UU 账号或第三方服务密钥。0.7起增加[Linux 原生主机 VA-API 编码](docs/native-encoding.md)，默认仍用软件编码，实际后端与回退在共享窗口显示。**原生主机仍是早期局域网实现，不兼容 Sunshine / GameStream 协议。**

## 界面

以下两张均为 Windows 实机截图。主界面的设备别名与壁纸使用客户端隐私显示；串流界面的远端运行专用演示画面。

**设备主界面**

![uu-client 设备主界面](docs/images/home-windows.png)

**串流界面**

![uu-client 串流界面与统计](docs/images/stream-windows.png)

## 已有功能

下表描述 UU 兼容模式。原生模式提供独立主机、图形端共享与显式授权的键鼠控制，具体范围见[原生串流说明](docs/native-streaming.md)。

| 方面 | 当前支持 |
|---|---|
| 账号与设备 | 扫码登录、取消或刷新二维码、设备列表与搜索、异步设备预览、本地收藏与最近使用、退出账号；退出后断开本客户端串流，保留软件偏好。 |
| 视频 | H.264；Windows D3D11VA、Linux VA-API 硬件解码，失败时回退 OpenH264。 |
| 画质 | 自动 / 预设 / 自定义码率，保持远端或指定分辨率，预设与自定义 5–144 FPS。实际帧率受被控端、屏幕和网络限制。 |
| 音频 | Opus 播放、音量、静音、限长缓冲与丢包平滑；顶部声音快捷按钮。 |
| 键盘与鼠标 | 标点与组合键、鼠标中键与拖动；桌面绝对定位 / 游戏相对移动，本地 / 远端光标。 |
| 多显示器 | 真实显示源标签、切屏、拖动排序；默认保持被控端显示布局，多设备串流窗口独立打开。 |
| 桌面体验 | 保留设备主窗口、窗口缩放、跨 DPI 适配、全屏串流、可隐藏悬浮菜单、居中统计 HUD。 |
| 剪贴板 | Windows 与 Linux 的前台串流双向文本同步，单次上限约 30 KiB。 |
| 网络恢复 | RTP 重排、NACK / RTX 重传、序号回绕与参考链恢复、限长呈现队列；尚未接入视频 FEC。 |

默认使用自动画质、60 FPS 目标、远端原分辨率，串流窗口在目标显示器工作区约 80% 范围内等比居中。设置中可以修改默认偏好；窗口右上角按钮负责最大化 / 还原，快捷键进入隐藏顶部栏的串流全屏。

0.6起提供[收藏与最近使用](docs/device-library.md)（0.5旧标签不含）；小窗口使用紧凑筛选栏，设备长名称按实际宽度省略并可悬停查看。

0.7修复了[丢包与重传跨 SRTP 序号回绕时可能永久停图的问题](docs/stream-recovery.md)，并保留加密认证与重放保护。该修复覆盖兼容模式接收端和原生模式；原生串流建议主机与控制端一起升级。0.8进一步修正Linux原生硬件编码与硬件解码的互通，减少BGRA转换开销，并在停图后及时将解码FPS归零。0.9候选将图形观看切换为 GPU 平面颜色转换，减少 CPU 与纹理上传工作；[延迟路径和计时说明](docs/windows-latency.md)明确保留的同步读回及测试范围。

## 开始使用

安装包随里程碑版本发布到 [GitHub Releases](https://github.com/TheRainstorm/uu-client/releases)，自动构建与标签规则见[版本发布说明](docs/releases.md)。

已有源码目录且已安装并登录 GitHub CLI、Python 3.11+ 时，可直接下载已发布产物，无需本机编译：

```sh
python3 tools/release/download.py
# 指定版本或平台：--tag v0.8.0 --platform windows
```

校验后的包统一位于 `artifacts/packages/`；工具核对发布标签对应提交、压缩包校验和及每个文件的哈希。下载不会安装或启动软件。

原生模式中，主机在主页选择“共享此设备”，选择显示器和本机网卡地址后开始共享；控制端打开“使用原生邀请”，选择私下传递的邀请文件。默认只读，控制键鼠需要主机明确许可。具体范围及 CLI 用法见[原生串流说明](docs/native-streaming.md)，全程无需 UU 账号或服务凭据。

v0.5.0 起的 UU 兼容模式内置 PC 客户端共享的请求签名值，首次使用无需手工创建签名文件；账号仍需扫码登录，远端操作仍受账号与设备权限控制。该共享值可从源码或程序中读取，不是用户个人令牌，也不代表获得官方接入授权。需要覆盖默认值时，见[连接签名配置说明](docs/connection-credentials.md)。v0.1–v0.4 发布包仍要求本地配置。

在你有权控制的远端设备上安装并配置官方被控端。启动 uu-client，点击扫码登录，使用手机完成登录后，从设备列表选择设备连接。主页在串流期间保留，可继续连接其它设备。

右上角提供账号、关于软件和齿轮设置入口；版本、构建提交和项目信息在“关于软件”中查看。登录态与设置仅保存在本地用户目录，安装包不包含账号数据。账号文件使用原子替换，Unix 权限仅当前用户可读写；调试输出不包含登录或二维码凭据。目前没有操作系统凭据保险库集成，应保护本机账户及配置文件。

0.6起会明确提示本地账号文件损坏、过大或不可读取，保留原文件并阻止重新注册覆盖；修复配置后可重试。短暂读取失败不自动退出已经建立的串流。
设置文件异常时使用内存默认值，自动保存不会覆盖原件；可在设置中主动备份异常文件后保存当前选项。

0.6起在 Windows / Linux 和软件解码路径中统一读取 H.264 的色彩范围与 BT.601 / BT.709 矩阵，避免因构建未启用 Linux 硬解而忽略色彩信息。

### Windows

在 Linux 上交叉编译并打包 Windows x64：

```bash
python3 tools/windows/package.py
```

将 `artifacts/packages/uu-client-windows-x64-<提交号>.zip` 复制到 Windows，解压后运行：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1
```

安装前退出旧客户端。安装后从桌面 **uu-client** 打开；更新会保留登录态与设置。工具链准备、自定义安装目录和校验方法见 [Windows 编译与打包](docs/windows-build.md)。

### Linux

安装 Rust 稳定版及系统开发依赖，构建原生客户端：

```bash
# Ubuntu / Debian
sudo apt install build-essential pkg-config cmake nasm libssl-dev \
  libasound2-dev libpulse-dev libva-dev libgbm-dev libclang-dev libxkbcommon-dev libxkbcommon-x11-dev
cargo run --release -p uu-gui --features vaapi --locked
```

生成含安装器的 Linux 包：

```bash
python3 tools/linux/package.py
```

解压后执行 `python3 install.py`，从应用菜单 **uu-client** 或 `~/.local/bin/uu-client` 启动。支持 Wayland / X11；运行库、驱动及平台差异见 [Linux 客户端说明](docs/linux-client.md)。

### 快捷键

以下字母键均与 **Ctrl+Alt** 同时按下：

| 按键 | 功能 |
|---|---|
| S | 显示 / 隐藏串流统计 |
| F 或 X | 进入 / 退出串流全屏 |
| Q | 退出当前串流 |
| M | 静音 / 恢复声音 |
| H | 显示 / 隐藏全屏悬浮入口 |
| Z | 捕获 / 释放鼠标 |

Windows 的 Win+Shift+方向键始终用于移动本地窗口。全屏系统快捷键捕获受操作系统限制；Ctrl+Alt+Del 由 Windows 保留。Wayland 是否允许快捷键抑制取决于合成器支持。

0.7起在 Windows 原生捕获路径中保留右 Alt / AltGr 作为远端输入，避免输入 `@` 等字符时误触退出或其它本地操作；本地快捷键请使用 Ctrl+左 Alt。

## 当前边界

- 原生被控端目前为实验性局域网共享通路，含 Windows DXGI / Linux X11 采集、默认软件 H.264 编码、Linux 实验性 VA-API 编码与可选系统声音；Wayland portal 与日常无人值守管理尚未接入该模式；键鼠控制需主机显式许可。不要把它当作完整的服务端。
- 视频目前只解码 H.264；H.265、AV1、HDR 与视频 FEC 未实现。高帧率选项不保证所有远端都能达到对应实际帧率。
- 文件传输、终端、端口映射和远程协助等入口尚未实现；界面中暂不可用的入口不代表现有能力。
- 硬解仍存在 CPU 回读，尚未做到完整零拷贝；网络波动、恢复与音频长稳体验仍需持续打磨。
- 服务兼容性依赖官方接口与被控端版本，可能随更新失效；未获官方认可或授权，账号与服务使用存在协议风险。

## 实现与开发

GUI 使用 egui / eframe，媒体面采用 WebRTC、H.264 与 Opus。协议、输入和呈现分层，账号配置与软件偏好分开保存。

```text
crates/uu-gui      设备主页、设置、串流窗口、输入捕获与呈现
crates/uu-core     账号 API、信令、WebRTC、显示源与恢复策略
crates/uu-proto    协议类型、控制消息与帧格式
crates/uu-media    解码、音频播放与媒体抽象
crates/uu-cli      调试与诊断命令 uu
crates/uu-capture  DXGI / X11 实际采集与合成测试源
crates/uu-input    按会话许可工作的 Windows / Linux 输入注入后端
tools/            构建、打包及隔离验证工具
vendor/           带有来源、许可与修改记录的局部依赖
```

```bash
cargo test --workspace --locked
cargo run -p uu-cli -- devices
cargo run -p uu-cli -- probe
```

详细文档：[串流质量与恢复](docs/streaming-quality.md) · [帧率协商](docs/frame-rate.md) · [功能与源码参考索引](docs/reference-index.md) · [协议笔记](docs/protocol-notes.md)。诊断输出可能包含设备标识或网络信息，提交 issue 前请自行清理。

## 方向与致谢

当前名称 **uu-client** 说明其兼容客户端定位。下一阶段计划以 **Eclipse / 日蚀** 为名，探索采集端与客户端一体化、自主串流协议与更完整的跨平台体验；原生局域网基础已实现，后续范围与验收顺序见[产品远景](docs/vision.md)和[交付路线图](docs/roadmap.md)。

感谢 [Moonlight](https://github.com/moonlight-stream/moonlight-qt)、[Sunshine](https://github.com/LizardByte/Sunshine)、[Foundation Sunshine](https://github.com/AlkaidLab/foundation-sunshine)、[uu-remote-linux](https://github.com/VallierDev/uu-remote-linux)，以及 Rust WebRTC、egui、OpenH264、Opus 等项目。参考机制与引用代码应分别遵守来源许可，具体参考版本和入口见源码参考索引。

## 许可证

本项目原创部分采用 **[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)** 双许可，由使用者任选其一，允许按许可商用。第三方组件保留各自许可证，见 [第三方通知](THIRD-PARTY-NOTICES.txt) 与 [依赖清单](docs/dependency-licenses.md)。

开源许可只涵盖维护者有权授予的代码权利，不授予官方服务接入、第三方商标或全部编解码专利许可。详见[许可与兼容接入说明](docs/licensing-and-compatibility.md)。
