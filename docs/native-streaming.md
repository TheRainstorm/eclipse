# 原生局域网串流（实验性）

v0.2.0 起的安装包包含此实验通路。当前开发分支新增默认的[局域网设备发现与持久配对](native-lan.md)，图形共享入口已切换为新通路。本文保留旧发布包及 CLI 邀请通路的使用方式。

这条通路使用本项目的主机与控制端，不连接 UU 服务、不需要 UU 账号或生产签名凭据。目前提供图形观看与 CLI 诊断：旧邀请入口及 CLI 默认软件 H.264 编码，当前开发分支的局域网“共享本机”默认自动选择硬编，支持 [Linux VA-API、Windows MFT 及 Intel VPL](native-encoding.md)。HEVC / AV1 需要 Intel VPL 硬件运行时；Windows H.264 VBR 可使用驱动 MFT，Linux VBR 仍使用 VPL。接收端需支持相同格式。控制端复用已有硬件解码、重排恢复与限长呈现队列。不是完整的日常远控服务端。与 UU 兼容模式的互通边界、UDP 传输及后续扩展见[协议与传输架构](protocol-compatibility-and-transport.md)。

0.7起在共享窗口提供本机网卡地址选择和刷新，也保留手动输入。默认仍为仅本机，刷新不会改变选择或启动监听；跨设备共享前需选择双方能互通的本机地址。暂不支持需要接口作用域的 IPv6 链路本地地址，地址列表也不会展示它们。

控制端在协商视频前创建解码后端，收到主机确认的分辨率后准备 Windows 解码组件，再启动媒体连接，减少驱动冷启动挤满接收队列的情况。驱动调用仍在各自专用线程；准备失败时回退软件解码。Linux VA-API 需要实际码流参数，相关配置仍在首个关键帧完成。

## 在本机验证

从源码构建 `cargo build --release -p uu-cli --locked`，或者使用包含这项功能的构建。以下命令中的 `uu` 是包里的 CLI，Windows 使用 `uu.exe`。

在第一个终端启动合成画面主机，不读取真实桌面：

```bash
uu native-host --synthetic --invitation-file ./demo.eclipse-invite.json
```

在第二个终端接收并解码120帧：

```bash
uu native-probe --invitation-file ./demo.eclipse-invite.json --frames 120
```

命令只输出解码统计，不默认保存图像。控制端结束后，该次主机会话也退出。邀请文件必须是新文件；重新启动时选择新的文件名。主机运行时可按 Ctrl+C 停止。

## 图形窗口

v0.3.0 起还可在主机的主页选择“共享此设备”，选择显示器、本机网卡 IP、帧率、码率与新的邀请文件路径，再点击“开始共享”。默认只允许观看；键鼠控制需要主机主动勾选许可。邀请默认10分钟内可连接，当前共享结束后自动清理本次邀请。跨设备时填写主机自身的局域网地址，回环地址只能本机使用。

启动 `uu-gui`，在控制端主页选择“使用原生邀请”或顶部“原生直连”，填写邀请文件路径后打开共享画面；也可把 `.eclipse-invite.json` 拖入主页。无需登录账号，主页会保留，每个邀请使用独立串流窗口。

也可直接启动：

```bash
uu-gui --native-invitation ./demo.eclipse-invite.json
```

窗口沿用80%工作区初始大小、居中、HUD与全屏快捷键：Ctrl+Alt+S显示统计，Ctrl+Alt+F / X切换全屏，Ctrl+Alt+Q退出当前串流。Wayland 的全局窗口位置由合成器安排，客户端会按所在输出调整尺寸；跨屏后的位置遵循桌面策略。该模式的菜单只显示已有能力；主机允许控制后，控制端可主动请求键鼠权限。本地快捷键始终生效；v0.5.0 起可按双方明确许可开启[文本剪贴板](native-clipboard.md)，原生会话不因账号模式登录或退出而断开。

## 局域网与真实显示器

默认信令和媒体都只绑定本机回环接口。跨设备时，主机使用 `--bind` 显式指定**自己的某个局域网网卡地址和端口**；不接受 `0.0.0.0`。将生成的邀请文件私下传给希望授权的控制端，再由控制端使用相同的 `native-probe` 命令验证。

去掉 `--synthetic` 才会使用当前交互会话的主屏。`uu native-sources` 可枚举显示器，再用 `native-host --monitor` 选择枚举名称。Windows 使用 DXGI Desktop Duplication，Linux 使用已授权 X11，或通过系统门户授权的 Wayland / PipeWire 采集；不会更改主屏、启用虚拟屏或关闭其它显示器。Wayland 由系统选择共享源，不用 Xwayland 根窗口代替真实桌面，具体授权与限制见[采集说明](native-capture.md)。

软件编码可用 `--fps`、`--bitrate-kbps` 设置，默认60fps、8000kbps；支持1–60fps及64–100000kbps。画面尺寸与帧率也必须符合协商的 H.264 level 5.2 上限。实际帧率取决于编码性能与网络，不承诺所有桌面尺寸均达到目标值。软件编码支持偶数尺寸，最大3840×2160或2160×3840；更大的源会提前提示，客户端不自动更改主机显示设置。

## 配对、范围与限制

- 停止共享、主窗口正常关闭或连接结束时，主机会清理自己生成的邀请文件；不会删除被其它程序替换为不同内容的文件。异常断电留下的邀请在主机退出后也无法连接，应自行删除。
- 邀请包含该次主机的证书和随机访问凭据，默认10分钟内可以配对；`--pairing-seconds` 最长1小时。已建立的会话不因邀请到期而中断。详见[原生配对基础](native-pairing.md)。
- TLS1.3固定主机身份并认证邀请后，才开放采集；SDP通过已认证通道交换，媒体使用 WebRTC DTLS-SRTP。没有自造明文远程桌面协议。
- 旧邀请通路不自动更改防火墙或开放公网入口，仅接受与认证信令对端地址一致的 UDP 主机候选。持久设备授权与多入口连接使用当前[设备工作区](device-connection-and-trust.md)；STUN 和公网寻址使用独立的[连接服务](native-internet.md)，不会由邀请文件隐式启用。
- v0.2.0 发布包为单控制端、单显示源的纯观看模式。v0.3.0 加入图形端共享与[显式授权的键鼠控制](native-input.md)，默认仍只读。v0.4.0 加入[系统声音与音频冗余恢复](native-audio.md)，v0.5.0 加入[双方许可文本剪贴板](native-clipboard.md)。当前开发分支的原生串流菜单已支持在授权范围内切换显示源；切换先释放输入，再重建媒体代数，不改变远端显示布局。Wayland 重新选择需要系统再次授权；运行中画质重协商仍待完善。
- `--max-seconds` 可限制已连接会话时长，便于隔离验证。当前开发分支 CLI 支持 `--encoder software|auto|hardware|vaapi`，默认 `software`；`hardware` 要求平台硬编成功，`vaapi` 仅适用于 Linux，Windows H.264 默认使用硬件 MFT。当前开发分支已接入 Windows MFT H.264 CBR / VBR 和 Intel VPL 的 H.264 / HEVC / AV1 与 CBR / VBR，详见[编码说明](native-encoding.md)；HDR 尚未实现；新原生两端增加有预算上限的 [XOR 视频 FEC](native-fec.md)，恢复同时保留 NACK 重传、PLI / FIR 与参考链保护。

0.7起 `native-probe` 可加 `--progress-seconds 5` 输出周期统计，以及 `--max-frame-gap-seconds 5` 在首帧后长时间没有可见帧时主动结束。两者默认关闭；与 `--report 新文件.json` 一起使用时，失败也会保留已收集的统计，释放本次连接。进度不包含画面、键鼠内容或邀请凭据，探针始终不播放接收到的声音。

## 隔离的 Wayland 窗口验证

安装 Sway、grim，并构建 `cargo build --release -p uu-gui --example wayland_input`。使用开发构建运行：

```bash
python3 tools/native/wayland_smoke.py \
  --gui ./target/release/uu-gui --cli ./target/release/uu \
  --input-driver ./target/release/examples/wayland_input \
  --output ./artifacts/native-wayland-test --multi-dpi
```

脚本启动自己的无头合成器、合成画面主机和观看窗口，检查全屏、只读模式下的本地快捷键、不同缩放比例输出间的往返尺寸及正常退出。输出目录必须不存在。默认使用 GLES；可用 `--renderer pixman`，验证 VA-API 时加 `--expect-hardware`。虚拟键盘只连接该脚本创建的 Wayland socket；不会向用户桌面发送按键。

## 诊断报告（v0.4.0 起）

`native-probe` 可生成新的私有 JSON 报告，适合较长串流或接收端丢包注入检查：

```bash
uu native-probe --invitation-file ./desktop.eclipse-invite.json \
  --frames 3600 --timeout-seconds 90 --report ./native-probe.json
```

报告包含逐秒帧率、带宽、解码耗时、RTT、丢包、NACK/RTX、坏帧、队列丢帧与关键帧恢复计数，以及首次取帧耗时和接收队列最大取帧间隔。它反映接收端观测，不是玻璃到玻璃延迟。超时也保留已经获得的指标，`completed` 为 false。报告不包含邀请、账号或桌面图像；不覆盖已有文件，Unix 创建权限为600。

主机会话正常结束时，CLI 还输出本地生产线程统计：`producer_ms` 为线程生命周期，`capture_us`、`encode_us`、`backpressure_us` 为累计采集、编码（包含像素转换）和发送队列等待的微秒数。分别除以 `captured` 可查看平均每帧开销，用来区分采集或编码瓶颈；它们不表示控制端解码或端到端延迟。当前开发分支在双方支持时另向客户端发送有界窗口内的阶段统计，供 HUD 与诊断报告查看，边界见[延迟统计](native-latency.md)。
