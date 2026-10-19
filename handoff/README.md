# 交接文档索引

按时间顺序编号，数字越大越新。新交接文档请接着编号，不要覆盖旧文档。

| 编号 | 文件 | 内容 |
|---|---|---|
| 01 | `01-base-handover.md` | 总纲：协议、登录、信令、输入、环境与机器表、历史逆向结论 |
| 02 | `02-2026-10-17-rust-turn.md` | TURN/PCK 打通，端到端首次出画面 |
| 03 | `03-2026-10-17-input.md` | 键鼠输入打通（echo 握手、输入通道） |
| 04 | `04-2026-10-18-bitrate.md` | 串流专用 UI + 码率控制（CaptureSettingRequest 字段含义） |
| 05 | `05-2026-10-19-hardware-decode.md` | 原生硬件解码：Windows D3D11VA / Linux VA-API + 回退与自测工具 |
| 06 | `06-2026-10-19-hardware-validation.md` | Intel/AMD 实机硬解验证、1088→1080 裁剪修复、Windows 分发包；x358h 新地址 |
| 07 | `07-2026-10-19-experience.md` | Windows 中键实测值 16、浅色参考版 GUI、RTP/NACK 恢复与显示队列优化；FEC 边界 |
| 08 | `08-2026-10-19-reference-index.md` | 功能拆分提交、三个本地项目源码参考索引、协议差异与后续实施顺序 |
| 09 | `09-2026-10-19-controls-rtx-auto.md` | 可隐藏悬浮菜单、官方自动档达18Mbps、RTX接收与严格恢复、Windows真实PTS |
| 10 | `10-2026-10-19-resize-hud-recovery-exit.md` | 窗口缩放、Moonlight HUD、侧边菜单确认、恢复竞争/高吞吐优化、退出清理 |
| 11 | `11-2026-10-19-connection-startup-unified-install.md` | CONTROL启动连接回归修复、迟到ICE回归测试、单一Windows安装入口与版本校验 |

| 12 | `12-2026-10-19-shortcuts-hud-dpi-window.md` | Ctrl+Alt+S、居中 HUD、删除旧设置、1:1 初始窗口与真实跨 DPI 往返、窗口最大化和串流全屏分离 |

| 13 | `13-2026-10-19-launcher-dpi-multiwindow.md` | 未串流主页原生 DPI/坐标修复、保留主页、多设备独立串流进程与清理隔离；用户自行实机验收 |

| 14 | `14-2026-10-19-startup-monitor-native-shortcuts.md` | 启动屏幕跟随光标、串流继承父屏、Windows 原生 Win+Shift+方向键跨屏、目标 DPI 最小尺寸更新 |

| 15 | `15-2026-10-19-frame-rate-super-screen.md` | 60/120/自定义fpsCount、初始化协商、屏幕类型与超级屏请求、移除硬解固定30帧元数据；用户实机验收 |

| 16 | `16-2026-10-19-quality-menu-control-input.md` | 官方样式画质菜单与FPS合并、移除超级屏操作、CONTROL未打开的输入兼容处理和实机待验边界 |

| 17 | `17-2026-10-19-dtls-reader-sctp.md` | DTLS握手结束接收死锁、SCTP长期connecting、修复前失败和修复后通过的回归、局部依赖补丁 |

| 18 | `18-2026-10-19-basic-demo.md` | 视频可用里程碑后的日常使用 demo：快捷键、80% 窗口、音频抗爆音、原生按键捕获、软件设置、两组鼠标模式、文本剪贴板及用户验收 |

| 19 | `19-2026-10-19-multi-display.md` | 真实显示器名称/主屏/轨道，UU样式顶部标签与全屏切屏，保持布局/可选虚拟屏独占及-2广播语义更正 |

| 20 | `20-2026-10-19-display-binding-install.md` | 修复未绑定显示器被误判超出5路，补齐原生屏幕激活，+紧随标签，正式入口迁移到D盘 |

约定：

- 当前多显示器修正与安装位置以[交接20](20-2026-10-19-display-binding-install.md)为准，布局策略见交接19，基本demo见交接18；实机实验仍由用户完成。

- 最新SCTP卡住的接收竞态与验证证据见 [交接17](17-2026-10-19-dtls-reader-sctp.md)；用户已确认`f74fc9d`恢复键鼠。交接16的初始化回退未解决用户实机超时，不再当作最终修复。

- 当前菜单见 [交接16](16-2026-10-19-quality-menu-control-input.md)：FPS合并画质，移除超级屏操作；该轮输入问题的后续修复与实机确认见交接17。

- 帧率协商、60/120/自定义的字段依据见 [交接15](15-2026-10-19-frame-rate-super-screen.md)，其中超级屏操作已在交接16移除。此前约30fps不代表官方host所有模式的统一上限；高帧率实机验收由用户完成。

- Windows 唯一正式入口为 `D:\SMB\uu-gui-demo\uu-gui.exe`，桌面 `UU Remote Rust`；旧C盘目录保留但不作为后续同步入口。当前启动屏幕与快捷键行为见交接14，多窗口行为见交接13，统一安装流程的最新路径见交接20。

- 当前 x358h SSH 地址是 `192.168.34.3:2202`（2026-10-19 用户更新）；历史文档中的 `192.168.35.200` 已失效。

- 协议与逆向证据从 01 开始读；后续实测更正优先于旧结论。功能参考入口见 [`docs/reference-index.md`](../docs/reference-index.md)。
- 迁移前的旧文件名（仓库里已不存在，仅供检索历史记录和旧对话时对照）：
  `HANDOVER.md` → 01，`HANDOVER-RUST-TURN-2026-10-17.md` → 02，
  `HANDOVER-RUST-INPUT-2026-10-17.md` → 03，
  `HANDOVER-RUST-BITRATE-2026-10-18.md` → 04。
