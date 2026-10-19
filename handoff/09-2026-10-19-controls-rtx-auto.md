# 09 · 悬浮控制、自动画质协商与 RTX 恢复

2026-10-19。当前 HEAD 以 `git log -1` 为准；用户要求每完成功能即提交。
参考图：用户 `/mnt/Disk1/uu_connect_top.png`、`uu_fullscreen_menu.png`，仅阅读，未分发官方素材。

## 本轮提交

| 提交 | 内容 |
|---|---|
| `eb23bbe` | 浅色连接顶栏、可拖动/隐藏的全屏悬浮入口、分组控制菜单与输入隔离 |
| `869107d` | H264 RTX 协商/恢复、应用窗口 NACK、默认严格参考链恢复、真实出图确认 |
| `2c7ff5a` | 补齐 DecoderCap、提高能力上限、默认官方自动档、分辨率选择 |
| `7f590ac` | D3D11 解码保留真实传输 PTS，不在重建后归零 |
| `1fae28a` | Windows 字体缺字修复，菜单图标改矢量绘制 |
| `9f69a3b` | 恢复时保留硬件失败预算，防止重建驱动清零回退条件 |

## 原因和改动

### 自动档与 6–7 Mbps

旧 ConnectOptions 没有 DecoderCap，streamer JSON 又声明固定1920×1080；GUI默认的是自定义55M，
与官方自动模式不一致。本轮增加H264 420的3840×2160@60解码能力声明，默认quality=5自动档，
设置中增加跟随远端、1080p、1440p、4K请求；显示仍以实际收到的码流尺寸为准。
自动码率仍由官方host和WebRTC反馈决定，不是客户端固定强推18M。

**纠正判断**：截图的“4K auto”是档位标签，不能据此认定线上视频就是4K。本轮新协商后
达到18.7Mbps的码流实际仍为1920×1080；不能把低码率只归因于视频尺寸。
能力声明、初始化设置和自动档的组合在本轮短测中恢复到官方相近量级，尚未逐字段做完整因果隔离。

### 花屏与恢复

此前webrtc-rs 0.11的repair reader读取RTX后丢弃；本项目也未声明RTX。现在对每个H264 PT
注册apt，按answer的FID SSRC和apt映射，在interceptor边界捕获RTX，恢复OSN/原PT/原SSRC，
通过512包有界通道进入同一VideoReceiver。不修改vendor库，不把RTX当作新视频帧计FPS。
相关定义参考 [RFC4588](https://www.rfc-editor.org/rfc/rfc4588)。

NACK不再交给8192包长期历史的默认generator：从当前重排窗口找缺包，5ms乱序宽限，20ms请求，
恢复/过期后停止重复请求；单次最多512个序号。重排仍默认60ms，顺序完好的帧不额外等待。
缺marker的静态尾帧也有200ms兜底，不等到下个画面才发现损坏。

之前默认“平衡模式”继续解依赖帧会容忍暂时的参考错误。现在默认严格恢复：参考链损坏后
等待IDR；只有当前代次的IDR实际产生图像后才确认恢复。`UU_STRICT_RECOVERY=0`可切回旧策略。
PLI每500ms限速，只发一个合法发送者SSRC；重复失败才升级FIR，移除重复/冒充远端SSRC的PLI。

D3D11原先自己合成60Hz时间戳，且重建后从0开始；30Hz输入会时钟漂移，恢复代次的PTS检查会
把新画面误认为旧帧，造成额外IDR请求。本轮将RTP派生PTS直接传入MF（微秒→100ns），跨重建保留。恢复重建也不再清零硬件失败预算，连续失败仍可转软解。

**FEC仍未实现**。RTX计数包括重复/探测包，不能直接理解为修复画面错误的次数。

### 控件

- 窗口态浅色36逻辑像素顶栏：设备、显示屏、吞吐、控制中心及窗口操作。
- 全屏悬浮入口可拖动；`Ctrl+Alt+M`隐藏/恢复，隐藏时快捷键仍可用。
- 菜单包含显示屏、画质/分辨率、窗口、安全/仅观看、更多/HUD、退出全屏和退出远控。
- 音频/外设/工具尚未接入，保持禁用；不显示为已支持。
- 菜单、子菜单和设置不穿透键鼠输入；按住的按钮/键在打开控制区时释放。
- Windows微软雅黑缺少部分装饰符号，改用矢量图标，避免菜单出现空框。

## 验证

只连接KVM-WIN11；x358h地址仍是`192.168.34.3:2202`。所有诊断显式`UU_FORCE_JOIN=0`。
新增 `cargo run -p uu-core --example stream_check -- aeawvmepembcpca5 35`：
只打印聚合媒体指标，设备固定限制为获准测试host；不打印按键/剪贴板/ICE凭证。
controller旧的ICE凭证输出也已改为只打印条数。

### x358h → KVM-WIN11

| 运行 | 结果 |
|---|---|
| 旧能力+auto、已启RTX | 实际1920×1080，前20s约4.9–5.0Mbps、30fps；后段出现突发缺包和恢复，非稳定零丢包基线 |
| 新能力+auto、修时钟之前 | 实际1920×1080，末段17.4–18.7Mbps，30fps，lost/bad/error均0；初始化队列丢3，刷新8 |
| 最终自动档，35s | 实际1920×1080，采样17.1–19.0Mbps，约30fps；最终lost=0、bad=0、error=0、queue_drop=0，初始refresh=1，RTX=176，末段不再等待IDR |
| 最终GUI拖动窗口/菜单 | 开专用WinForms测试窗，远程左键按住36次移动，退出全屏/打开菜单/隐藏恢复；截图及日志中lost/bad/error均0；初始化queue_drop=3，refresh=3 |

这些是同一噪点压力页附近的连续短测，不是严格同步A/B或长稳保证。专用窗口遮挡大部分动态内容后
GUI吞吐降到0.1–0.5Mbps是内容变化，不能与全屏噪点时18Mbps直接比较。
仍有CPU NV12→RGBA、GPU回读/上传和UI节拍开销，不声称体验已完全等同官方客户端。

### 自动检查

- 最终 `cargo test --workspace --locked`：78项通过。
- RTX测试含apt/FID拒绝、OSN跨回绕、FU重组、重复去重；NACK测试含宽限、修复后停止、过期和静态缺尾。
- Windows硬解PTS测试：Intel Arc B390、AMD RX7900XTX分别1项通过；输入从10s/20s起、30Hz、重建两次，输出必须来自已提交PTS集合。
- Windows硬件测试默认ignored，需显式在测试GPU运行：
  `cargo test -p uu-media -- --ignored --exact hw::windows_timing_tests::hardware_output_keeps_transport_pts_across_decoder_recreation`。
- Windows GUI/CLI release构建通过；Linux VAAPI没有新增真机验证。

私有脚本、聚合结果、截图在`artifacts/menu-stream-validation/`，不提交真实桌面截图。
第一轮Windows截图已查看并确认菜单/拖动功能。最后的矢量图标复查遇到x358h自动锁屏，
`connection-top-latest.png`实际截到Windows锁屏；后续同名final PNG不作为有效GUI验收证据。
不修改锁屏策略、不尝试解锁；最终矢量图标包的外观需要解锁后复查。
测试计划任务和专用窗口在验收后清理，保留用户原有程序。

## 后续

1. 可控丢包/突发/乱序、长稳和真实拖动A/B，统计有效修复包、IDR恢复p95及呈现间隔。
2. GPU纹理直接呈现和源PTS驱动的frame pacing；现在仍有CPU回读/转换，GUI不是Moonlight完整呈现链。
3. 画质/分辨率RPC实际应用回报；继续逐字段核对官方自动档与能力声明。
4. HEVC/444、音频、光标、剪贴板等保持独立任务；FEC须确认官方线上格式再实现。

## 分发包

`dist/uu-gui-win64/`已更新，两个exe与README一同分发。SHA-256：

- `uu-gui.exe`：`6eccbe7f926562ba5d96e1a8b7e00a13a8e2a99464f91ba50eaab73d6e114e66`
- `uu.exe`：`659213dac987d84ef865870621394719bded960027dcfb7941a3c0e4567869e3`

x358h `C:\Users\yfy\uu-gui-demo\`根目录和`rtx-auto\`子目录均已更新，远端SHA-256一致。
保留此前运行中的用户GUI进程和旧exe备份，没有结束用户程序；需重开GUI才能使用新二进制。
